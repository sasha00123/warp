use super::*;
use std::os::unix::fs::PermissionsExt;

fn fake_command(script: &str) -> (Backend, PathBuf) {
    let directory = std::env::temp_dir().join(format!("ew-command-{}", random_token().unwrap()));
    std::fs::create_dir(&directory).unwrap();
    let executable = directory.join("tmux");
    std::fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut backend = Backend::new("ew-command-test").unwrap();
    backend.executable = executable;
    backend.timeout = Duration::from_millis(100);
    (backend, directory)
}

#[test]
fn inherited_output_does_not_wait_for_daemon_eof() {
    let (backend, directory) = fake_command("printf complete; sleep 2 &");
    let start = Instant::now();
    let result = backend.run(&[]);
    std::fs::remove_dir_all(directory).unwrap();
    assert_eq!(result.unwrap(), "complete");
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn stalled_command_obeys_deadline_without_output_eof() {
    let (backend, directory) = fake_command("exec sleep 2");
    let start = Instant::now();
    let result = backend.run(&[]);
    std::fs::remove_dir_all(directory).unwrap();
    assert_eq!(result.unwrap_err().kind, ErrorKind::Timeout);
    assert!(start.elapsed() < Duration::from_secs(1));
}

struct Lab(Backend);

impl Lab {
    fn new() -> Self {
        let backend = Backend::new(&format!("ew-test-{}", random_token().unwrap())).unwrap();
        backend.probe().expect("Tests require tmux >= 3.2");
        Self(backend)
    }

    fn create(&self) -> Workspace {
        self.0.create(&random_token().unwrap(), None).unwrap()
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = self.0.run(&["kill-server"]);
    }
}

#[test]
fn rejects_unsafe_ids_before_invoking_tmux() {
    let mut backend = Backend::new("ew-test-invalid").unwrap();
    backend.executable = "/does-not-exist/tmux".into();
    for id in [
        "",
        "@1",
        "$(touch /tmp/bad)",
        "a; kill-server",
        "../a",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        assert_eq!(
            backend.create(id, None).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
        assert_eq!(
            backend.terminate(id, id).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
    }
}

#[test]
fn rejects_unsafe_socket_names() {
    for socket in ["", "../warp", "/tmp/warp", "x y", "x;y"] {
        assert!(Backend::new(socket).is_err());
    }
}

#[test]
fn missing_tmux_is_not_an_empty_workspace_list() {
    let mut backend = Backend::new("ew-test-unavailable").unwrap();
    backend.executable = "/does-not-exist/tmux".into();
    let error = backend.list().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable, "{error:?}");
}

#[test]
fn missing_tmux_in_path_is_actionable() {
    let mut backend = Backend::new("ew-test-unavailable").unwrap();
    backend.executable = format!("ew-missing-tmux-{}", random_token().unwrap()).into();
    let error = backend.list().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable, "{error:?}");
    assert!(error.message.contains("tmux 3.2"));
    assert!(error.message.contains("remote SSH PATH"));
}

#[test]
fn silent_tmux_failure_has_an_actionable_diagnostic() {
    let (backend, directory) = fake_command("exit 1");
    let error = backend.list().unwrap_err();
    std::fs::remove_dir_all(directory).unwrap();
    assert_eq!(error.kind, ErrorKind::Failed, "{error:?}");
    assert!(error.message.contains("without a diagnostic"));
    assert!(error.message.contains("installed and executable"));
}

#[test]
fn validates_tmux_numeric_identifiers() {
    assert!(numeric_id("$123", '$'));
    for value in ["", "$", "$1;kill-server", "@2", "$-1"] {
        assert!(!numeric_id(value, '$'));
    }
}

#[test]
#[ignore = "requires an isolated Unix host with tmux"]
fn new_workspace_and_create_retry_keep_identity() {
    let lab = Lab::new();
    assert!(lab.0.list().unwrap().is_empty());
    let workspace = lab.create();
    assert_eq!(lab.0.create(&workspace.id, None).unwrap(), workspace);
    assert_eq!(lab.0.list().unwrap(), vec![workspace]);
}

#[test]
#[ignore = "requires an isolated Unix host with tmux"]
fn concurrent_retries_create_only_one_workspace() {
    let lab = Lab::new();
    let id = random_token().unwrap();
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let backend = lab.0.clone();
            let id = id.clone();
            thread::spawn(move || backend.create(&id, None).unwrap())
        })
        .collect();
    let workspaces: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert!(
        workspaces
            .iter()
            .all(|workspace| workspace == &workspaces[0])
    );
    assert_eq!(lab.0.list().unwrap().len(), 1);
}

#[test]
#[ignore = "requires an isolated Unix host with tmux"]
fn recreated_backend_resolves_same_live_process() {
    let lab = Lab::new();
    let workspace = lab.create();
    lab.0
        .run(&[
            "send-keys",
            "-t",
            &workspace.pane_id,
            "while true; do date +%s; sleep 0.1; done",
            "Enter",
        ])
        .unwrap();
    thread::sleep(Duration::from_millis(250));
    let backend = Backend::new(&lab.0.socket).unwrap();
    assert_eq!(
        backend
            .resolve(&workspace.id, &workspace.generation)
            .unwrap(),
        workspace
    );
    let before = backend
        .run(&["capture-pane", "-p", "-t", &workspace.pane_id])
        .unwrap();
    thread::sleep(Duration::from_millis(1200));
    let after = backend
        .run(&["capture-pane", "-p", "-t", &workspace.pane_id])
        .unwrap();
    assert_ne!(before, after);
    backend
        .run(&["send-keys", "-t", &workspace.pane_id, "C-c"])
        .unwrap();
}

#[test]
#[ignore = "requires an isolated Unix host with tmux"]
fn stale_generation_cannot_resolve_or_delete() {
    let lab = Lab::new();
    let workspace = lab.create();
    let stale = random_token().unwrap();
    assert_eq!(
        lab.0.resolve(&workspace.id, &stale).unwrap_err().kind,
        ErrorKind::Stale
    );
    assert_eq!(
        lab.0.terminate(&workspace.id, &stale).unwrap_err().kind,
        ErrorKind::Stale
    );
    assert_eq!(lab.0.list().unwrap(), vec![workspace]);
}

#[test]
#[ignore = "requires an isolated Unix host with tmux"]
fn server_restart_invalidates_old_reference_even_when_numeric_ids_repeat() {
    let lab = Lab::new();
    let original = lab.create();
    lab.0.run(&["kill-server"]).unwrap();
    thread::sleep(Duration::from_millis(100));
    let replacement = lab.0.create(&original.id, None).unwrap();
    assert_eq!(replacement.pane_id, original.pane_id);
    assert_ne!(replacement.generation, original.generation);
    assert_eq!(
        lab.0
            .terminate(&original.id, &original.generation)
            .unwrap_err()
            .kind,
        ErrorKind::Stale
    );
    assert_eq!(
        lab.0
            .resolve(&replacement.id, &replacement.generation)
            .unwrap(),
        replacement
    );
}

#[test]
#[ignore = "requires an isolated Unix host with tmux"]
fn terminating_one_workspace_does_not_kill_another() {
    let lab = Lab::new();
    let first = lab.create();
    let second = lab.create();
    lab.0.terminate(&first.id, &first.generation).unwrap();
    assert_eq!(
        lab.0
            .resolve(&first.id, &first.generation)
            .unwrap_err()
            .kind,
        ErrorKind::NotFound
    );
    assert_eq!(
        lab.0.resolve(&second.id, &second.generation).unwrap(),
        second
    );
}

#[test]
#[ignore = "requires an isolated Unix host with tmux"]
fn working_directory_is_passed_as_data_not_shell_code() {
    let lab = Lab::new();
    let directory = std::env::temp_dir().join(format!(
        "ew-{} spaces; dollar$ quote'",
        random_token().unwrap()
    ));
    std::fs::create_dir(&directory).unwrap();
    let workspace = lab
        .0
        .create(&random_token().unwrap(), Some(&directory))
        .unwrap();
    let cwd = lab
        .0
        .run(&[
            "display-message",
            "-p",
            "-t",
            &workspace.pane_id,
            "#{pane_current_path}",
        ])
        .unwrap();
    assert_eq!(
        Path::new(cwd.trim()).canonicalize().unwrap(),
        directory.canonicalize().unwrap()
    );
    lab.0
        .terminate(&workspace.id, &workspace.generation)
        .unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
#[ignore = "requires an isolated Unix host with tmux"]
fn refuses_to_adopt_unmanaged_session_with_matching_name() {
    let lab = Lab::new();
    let id = random_token().unwrap();
    let name = format!("{PREFIX}{id}");
    lab.0.run(&["new-session", "-d", "-s", &name]).unwrap();
    assert_eq!(lab.0.create(&id, None).unwrap_err().kind, ErrorKind::Failed);
}
