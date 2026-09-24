//! Opt-in integration: canonical Warp assets, real tmux/shell, native parser.
#![allow(clippy::disallowed_types)]
use super::*;
use crate::terminal::model::ansi::Processor;
use crate::terminal::persistent_tty::bootstrap::profile_for_shell;
use remote_server::persistent_workspace::{Backend, BootstrapProfile, Workspace};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Lab { backend: Backend, socket: String, root: PathBuf }
impl Lab {
    fn new() -> Self {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let socket = format!("ew-native-{nonce:x}");
        Self { backend: Backend::new(&socket).unwrap(), socket,
            root: std::env::temp_dir().join(format!("ew-native-{nonce:x}")) }
    }
    fn create(&self, shell: ShellType) -> Workspace {
        let profile = profile_for_shell(shell).unwrap();
        let helper = std::env::var_os("ETERNALWARP_TEST_RECORDER")
            .expect("Set ETERNALWARP_TEST_RECORDER to the VM harness recorder executable");
        self.backend.create_initialized(&"b".repeat(32), None, BootstrapProfile {
            shell: &profile.shell, init_script: &profile.init_script,
            bootstrap_script: &profile.bootstrap_script,
        }, &PathBuf::from(helper), &self.root).unwrap()
    }
    fn consume_until(&self, workspace: &Workspace, model: &mut TerminalModel, parser: &mut Processor,
        cursor: &mut u64, ready: impl Fn(&TerminalModel) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut raw = Vec::new();
        loop {
            let page = self.backend.read_output(&workspace.id, &workspace.generation, &self.root, *cursor).unwrap();
            assert!(!page.history_gap);
            parser.parse_bytes(model, &page.bytes, &mut std::io::sink());
            raw.extend(page.bytes);
            *cursor = page.next_cursor;
            if ready(model) { return; }
            assert!(Instant::now() < deadline,
                "Native state did not converge: command={:?}, state={:?}, raw={:?}",
                model.block_list().active_block().command_to_string(),
                model.block_list().active_block().state(), String::from_utf8_lossy(&raw));
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["-L", &self.socket, "kill-server"]).status();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn verify_native_shell(shell: ShellType) {
    let lab = Lab::new();
    let workspace = lab.create(shell);
    let session_id = SessionId::from(remote_server::persistent_shell::session_id(&workspace.generation).unwrap());
    let new_model = || {
        let mut model = TerminalModel::mock(None, None);
        model.enable_persistent_workspace_mode();
        model.register_session_id(session_id);
        model
    };
    let mut model = new_model();
    let mut parser = Processor::new();
    let mut cursor = 0;
    lab.consume_until(&workspace, &mut model, &mut parser, &mut cursor, |model| {
        model.is_active_block_bootstrapped()
            && model.block_list().active_block().session_id() == Some(session_id)
    });
    assert_eq!(remote_server::persistent_shell::activity(&lab.root, &workspace.id, &workspace.generation), Some("idle"));
    let command: &[u8] = if shell == ShellType::Fish {
        b"while true; printf 'NATIVE-OFFLINE-JOB\\n'; sleep 0.2; end\r"
    } else {
        b"while :; do printf 'NATIVE-OFFLINE-JOB\\n'; sleep 0.2; done\r"
    };
    lab.backend.send_input(&workspace.id, &workspace.generation, command).unwrap();
    lab.consume_until(&workspace, &mut model, &mut parser, &mut cursor, |model| {
        model.block_list().active_block().state() == BlockState::Executing
            && model.block_list().active_block().command_to_string().contains("NATIVE-OFFLINE-JOB")
    });
    // Simulate destroying every local parser/model while the remote job runs.
    assert_eq!(remote_server::persistent_shell::activity(&lab.root, &workspace.id, &workspace.generation), Some("running"));
    drop(model);
    std::thread::sleep(Duration::from_millis(300));
    let mut reopened = new_model();
    let mut parser = Processor::new();
    let mut cursor = 0;
    lab.consume_until(&workspace, &mut reopened, &mut parser, &mut cursor, |model| {
        model.block_list().active_block().state() == BlockState::Executing
            && model.block_list().active_block().command_to_string().contains("NATIVE-OFFLINE-JOB")
    });
    assert_eq!(lab.backend.resolve(&workspace.id, &workspace.generation).unwrap().shell_pid, workspace.shell_pid);
    lab.backend.send_input(&workspace.id, &workspace.generation, &[3]).unwrap();
    lab.consume_until(&workspace, &mut reopened, &mut parser, &mut cursor, |model| {
        // Stock macOS Bash 3.2 reports 1 for SIGINT interrupting this compound
        // loop (confirmed against the same loop without Warp). Preserve its
        // actual status rather than manufacturing 130 in the terminal model.
        let status = model.block_list().previous_command_exit_code();
        (status == Some(ExitCode::from(130))
            || (shell == ShellType::Bash && status == Some(ExitCode::from(1))))
            && model.block_list().active_block().state() != BlockState::Executing
    });
    assert_eq!(remote_server::persistent_shell::activity(&lab.root, &workspace.id, &workspace.generation), Some("idle"));
}

#[test]
#[ignore = "requires the isolated macOS VM and its matching recorder harness"]
fn persistent_native_bash_bootstrap_job_replay_and_interrupt() {
    verify_native_shell(ShellType::Bash);
}

#[test]
#[ignore = "requires the isolated macOS VM and its matching recorder harness"]
fn persistent_native_zsh_bootstrap_job_replay_and_interrupt() {
    verify_native_shell(ShellType::Zsh);
}

#[test]
#[ignore = "requires the isolated macOS VM, Fish, and its matching recorder harness"]
fn persistent_native_fish_bootstrap_job_replay_and_interrupt() {
    verify_native_shell(ShellType::Fish);
}

#[test]
#[ignore = "requires the isolated macOS VM and its matching recorder harness"]
fn persistent_native_shell_exit_finishes_its_block_without_closing_the_view() {
    let lab = Lab::new();
    let workspace = lab.create(ShellType::Bash);
    let session_id = SessionId::from(remote_server::persistent_shell::session_id(&workspace.generation).unwrap());
    let mut model = TerminalModel::mock(None, None);
    model.enable_persistent_workspace_mode();
    model.register_session_id(session_id);
    let mut parser = Processor::new();
    let mut cursor = 0;
    lab.consume_until(&workspace, &mut model, &mut parser, &mut cursor, |model| model.is_active_block_bootstrapped());
    lab.backend.send_input(&workspace.id, &workspace.generation, b"exit 7\r").unwrap();
    lab.consume_until(&workspace, &mut model, &mut parser, &mut cursor, |model| {
        model.block_list().active_block().command_to_string().contains("exit 7")
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let exit_code = loop {
        let current = lab.backend.resolve(&workspace.id, &workspace.generation).unwrap();
        if let Some(code) = current.exit_code { assert!(current.exited); break code; }
        assert!(Instant::now() < deadline, "tmux did not report shell exit status");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(exit_code, 7);
    model.finish_persistent_shell(exit_code);
    assert!(model.block_list().active_block().finished());
    assert!(!model.block_list().active_block().is_executing());
    assert!(!model.is_read_only(), "Finishing a retained block must not dispose its terminal view");
}
