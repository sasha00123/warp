#![cfg(unix)]
#![allow(clippy::disallowed_types)]
use persistent_workspace_rpc_harness::persistent_workspace::{Backend, BootstrapProfile, Workspace};
use persistent_workspace_rpc_harness::persistent_shell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Lab { backend: Backend, socket: String, root: PathBuf }
impl Lab {
    fn new() -> Self {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let socket = format!("ew-start-{nonce:x}");
        Self { backend: Backend::new(&socket).unwrap(), socket,
            root: std::env::temp_dir().join(format!("ew-start-{nonce:x} # quote'")) }
    }
    fn create(&self, shell: &str) -> Workspace {
        self.backend.create_initialized(&"a".repeat(32), None, profile(shell),
            Path::new(env!("CARGO_BIN_EXE_recorder")), &self.root).unwrap()
    }
    fn output(&self, workspace: &Workspace, marker: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let page = self.backend.read_output(&workspace.id, &workspace.generation, &self.root, 0).unwrap();
            let text = String::from_utf8_lossy(&page.bytes).into_owned();
            if text.contains(marker) { return text; }
            assert!(Instant::now() < deadline, "Missing {marker:?}: {text:?}");
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
fn profile(shell: &str) -> BootstrapProfile<'_> {
    BootstrapProfile {
        shell,
        init_script: br#"WARP_SESSION_ID=@@WARP_SESSION_ID@@; printf 'INIT:%s\n' "$WARP_SESSION_ID""#,
        bootstrap_script: b"export EW_STARTUP_VALUE=preserved; stty -echo; PS1=''; printf 'READY\\n'\n",
    }
}

#[test]
#[ignore = "requires an isolated Unix VM with tmux"]
fn startup_is_recorded_and_create_retry_does_not_bootstrap_a_running_job() {
    let lab = Lab::new();
    let workspace = lab.create("bash");
    let initial = lab.output(&workspace, "READY");
    assert!(initial.contains(&format!("INIT:{}", persistent_shell::session_id(&workspace.generation).unwrap())));
    lab.backend.send_input(&workspace.id, &workspace.generation,
        b"while :; do printf 'JOB:%s\\n' \"$EW_STARTUP_VALUE\"; sleep 0.1; done\r").unwrap();
    lab.output(&workspace, "JOB:preserved");
    let retried = lab.create("bash");
    assert_eq!(workspace, retried);
    std::thread::sleep(Duration::from_millis(250));
    let output = lab.output(&workspace, "JOB:preserved");
    assert_eq!(output.matches("INIT:").count(), 1);
    assert_eq!(output.matches("READY").count(), 1);
    assert!(output.matches("JOB:preserved").count() >= 2);
    lab.backend.send_input(&workspace.id, &workspace.generation, &[3]).unwrap();
}

#[test]
#[ignore = "requires an isolated Unix VM with tmux"]
fn concurrent_initialized_create_retries_keep_one_shell_and_one_bootstrap() {
    let lab = Lab::new();
    let workspaces = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4).map(|_| scope.spawn(|| lab.create("bash"))).collect();
        handles.into_iter().map(|handle| handle.join().unwrap()).collect::<Vec<_>>()
    });
    assert!(workspaces.iter().all(|workspace| workspace == &workspaces[0]));
    let output = lab.output(&workspaces[0], "READY");
    assert_eq!(output.matches("INIT:").count(), 1);
}

#[test]
#[ignore = "requires an isolated Unix VM with tmux and zsh"]
fn zsh_uses_the_same_recorder_gated_startup() {
    let lab = Lab::new();
    let workspace = lab.create("zsh");
    let output = lab.output(&workspace, "READY");
    assert!(output.contains("INIT:"));
    lab.backend.send_input(&workspace.id, &workspace.generation,
        b"printf 'ZSH-VALUE:%s\\n' \"$EW_STARTUP_VALUE\"\r").unwrap();
    lab.output(&workspace, "ZSH-VALUE:preserved");
}

#[test]
#[ignore = "requires an isolated Unix VM with tmux"]
fn retention_survives_create_retry_and_history_deletes_only_after_termination() {
    use persistent_workspace_rpc_harness::persistent_journal::RetentionPolicy;
    for (policy, version) in [(RetentionPolicy::UntilWorkspaceDeleted, "EWJ2"), (RetentionPolicy::Rolling, "EWJ1")] {
        let lab = Lab::new();
        let id = "a".repeat(32);
        let workspace = lab.backend.create_initialized_with_retention(&id, None, profile("bash"),
            Path::new(env!("CARGO_BIN_EXE_recorder")), &lab.root, policy).unwrap();
        lab.output(&workspace, "READY");
        let journal = lab.root.join(format!("{}-{}", workspace.id, workspace.generation));
        assert!(std::fs::read_to_string(journal.join("manifest")).unwrap().starts_with(version));
        let retry = lab.create("bash");
        assert_eq!(retry, workspace);
        assert!(std::fs::read_to_string(journal.join("manifest")).unwrap().starts_with(version));
        assert!(lab.backend.delete_retained_history(&id, &workspace.generation, &lab.root).is_err());
        assert!(journal.is_dir());
        lab.backend.terminate(&id, &workspace.generation).unwrap();
        lab.backend.delete_retained_history(&id, &workspace.generation, &lab.root).unwrap();
        assert!(!journal.exists());
        assert!(!lab.root.join(format!("{id}-{}.shell", workspace.generation)).exists());
        lab.backend.delete_retained_history(&id, &workspace.generation, &lab.root).unwrap();
    }
}
