//! Opt-in tests: actual tmux, actual production recorder, no mock terminal.
#![cfg(unix)]
// This dependency-free VM runner never targets Windows or wasm.
#![allow(clippy::disallowed_types)]
use persistent_workspace_rpc_harness::persistent_workspace::{Backend, ErrorKind, Workspace};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Lab {
    backend: Backend,
    socket: String,
    root: PathBuf,
}
impl Lab {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            + NEXT.fetch_add(1, Ordering::Relaxed) as u128;
        let socket = format!("ew-stream-{nonce:x}");
        Self {
            backend: Backend::new(&socket).unwrap(),
            socket,
            root: std::env::temp_dir().join(format!("ew-output-{nonce:x} # quote' spaces")),
        }
    }
    fn create(&self, id: u128) -> Workspace {
        let workspace = self.backend.create(&format!("{id:032x}"), None).unwrap();
        // Do not capture command echo as fake output. Native shell bootstrap will
        // own echo policy in the integrated app; the transport test sets it here.
        self.input(&workspace, b"stty -echo\r");
        std::thread::sleep(Duration::from_millis(150));
        self.backend
            .start_recording(
                &workspace.id,
                &workspace.generation,
                &PathBuf::from(env!("CARGO_BIN_EXE_recorder")),
                &self.root,
            )
            .unwrap();
        workspace
    }
    fn input(&self, workspace: &Workspace, bytes: &[u8]) {
        self.backend
            .send_input(&workspace.id, &workspace.generation, bytes)
            .unwrap();
    }
    fn wait_for(&self, workspace: &Workspace, cursor: &mut u64, needle: &[u8]) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        loop {
            let page = self
                .backend
                .read_output(&workspace.id, &workspace.generation, &self.root, *cursor)
                .unwrap();
            assert!(!page.history_gap);
            assert_eq!(page.start_cursor, *cursor);
            *cursor = page.next_cursor;
            output.extend(page.bytes);
            if output.windows(needle.len()).any(|window| window == needle) {
                return output;
            }
            assert!(
                Instant::now() < deadline,
                "Missing {:?} in {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&output)
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .status();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
#[ignore = "requires an isolated Unix VM with tmux"]
fn raw_output_offline_job_reconnect_interrupt_stdin_resize_and_exit() {
    let lab = Lab::new();
    let workspace = lab.create(1);
    let mut cursor = 0;
    lab.input(&workspace, br#"printf '\033]9278;f;{"hook":"test"}\007'; i=0; while :; do printf 'TICK:%s\n' "$i"; i=$((i+1)); sleep 0.1; done"#);
    lab.input(&workspace, b"\r");
    let output = lab.wait_for(&workspace, &mut cursor, b"TICK:1");
    let hook = b"\x1b]9278;f;{\"hook\":\"test\"}\x07";
    assert!(
        output.windows(hook.len()).any(|bytes| bytes == hook),
        "{output:?}"
    );
    // No read or SSH-like backend is required while the job emits output.
    let disconnected_cursor = cursor;
    std::thread::sleep(Duration::from_millis(450));
    let reconnected = Backend::new(&lab.socket).unwrap();
    assert_eq!(
        reconnected
            .resolve(&workspace.id, &workspace.generation)
            .unwrap()
            .shell_pid,
        workspace.shell_pid
    );
    reconnected
        .start_recording(
            &workspace.id,
            &workspace.generation,
            // Replacing the extension executable must not prevent reuse of a
            // recorder already owned by tmux, nor launch a second recorder.
            &lab.root.join("replaced-extension (deleted)"),
            &lab.root,
        )
        .unwrap();
    let page = reconnected
        .read_output(
            &workspace.id,
            &workspace.generation,
            &lab.root,
            disconnected_cursor,
        )
        .unwrap();
    assert!(!page.bytes.is_empty());
    assert_eq!(page.start_cursor, disconnected_cursor);
    cursor = page.next_cursor;
    lab.input(&workspace, &[3]);
    lab.backend
        .resize(&workspace.id, &workspace.generation, 101, 43)
        .unwrap();
    lab.input(&workspace, b"printf '\\nSIZE:'; stty size; printf 'INPUT:'; read -r value; printf '\\nRECEIVED:%s\\n' \"$value\"\r");
    let output = lab.wait_for(&workspace, &mut cursor, b"INPUT:");
    assert!(String::from_utf8_lossy(&output).contains("SIZE:43 101"));
    lab.input(&workspace, b"still interactive\r");
    lab.wait_for(&workspace, &mut cursor, b"RECEIVED:still interactive");
    lab.input(&workspace, b"exit 7\r");
    let deadline = Instant::now() + Duration::from_secs(3);
    while lab
        .backend
        .resolve(&workspace.id, &workspace.generation)
        .unwrap()
        .exit_code
        .is_none()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    let exited = lab
        .backend
        .resolve(&workspace.id, &workspace.generation)
        .unwrap();
    assert!(exited.exited);
    assert_eq!(exited.exit_code, Some(7));
    assert!(
        lab.backend
            .read_output(&workspace.id, &workspace.generation, &lab.root, 0)
            .unwrap()
            .high_watermark
            > cursor
    );
}

#[test]
#[ignore = "requires an isolated Unix VM with tmux"]
fn shell_exit_flushes_all_trailing_output_before_journal_completion() {
    let lab = Lab::new();
    let workspace = lab.create(3);
    // Interactive shells may echo a submitted command even after stty -echo.
    // Keep the output marker absent from that command's literal text.
    lab.input(
        &workspace,
        b"awk 'BEGIN { for (i=0; i<60000; i++) printf \"TRAIL%s%06d\\n\", \":\", i }'; exit 7\r",
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut cursor = 0;
    let mut output = Vec::new();
    let mut pages = 0;
    loop {
        let page = lab
            .backend
            .read_output(&workspace.id, &workspace.generation, &lab.root, cursor)
            .unwrap();
        assert!(!page.history_gap);
        assert_eq!(page.start_cursor, cursor);
        cursor = page.next_cursor;
        output.extend(page.bytes);
        pages += 1;
        if page.closed && cursor == page.high_watermark {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Final output was never durably closed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pages > 1);
    let text = String::from_utf8(output).unwrap();
    assert_eq!(text.matches("TRAIL:").count(), 60000);
    let expected: String = (0..60000)
        .map(|index| format!("TRAIL:{index:06}\r\n"))
        .collect();
    assert!(
        text.contains(&expected),
        "The final output must contain every line in order, without duplicates or omissions"
    );
    assert_eq!(
        lab.backend
            .resolve(&workspace.id, &workspace.generation)
            .unwrap()
            .exit_code,
        Some(7)
    );
    // Older tmux can retain pane_pipe after the idle recorder has exited.
    // The recorder's final manifest, not that flag, proves output completion.
    let final_page = lab
        .backend
        .read_output(&workspace.id, &workspace.generation, &lab.root, cursor)
        .unwrap();
    assert!(final_page.closed);
    assert_eq!(final_page.high_watermark, cursor);
    assert!(final_page.bytes.is_empty());
}

#[test]
#[ignore = "requires an isolated Unix VM with tmux"]
fn switching_cursors_and_termination_do_not_mix_workspaces() {
    let lab = Lab::new();
    let first = lab.create(1);
    let second = lab.create(2);
    lab.input(&first, b"printf 'ONLY_FIRST\\n'\r");
    lab.input(&second, b"printf 'ONLY_SECOND\\n'\r");
    let mut first_cursor = 0;
    let mut second_cursor = 0;
    let output = lab.wait_for(&first, &mut first_cursor, b"ONLY_FIRST");
    assert!(!String::from_utf8_lossy(&output).contains("ONLY_SECOND"));
    let output = lab.wait_for(&second, &mut second_cursor, b"ONLY_SECOND");
    assert!(!String::from_utf8_lossy(&output).contains("ONLY_FIRST"));
    assert_eq!(
        lab.backend
            .send_input(&first.id, &"0".repeat(32), b"exit\r")
            .unwrap_err()
            .kind,
        ErrorKind::Stale
    );
    lab.backend.terminate(&first.id, &first.generation).unwrap();
    lab.input(&second, b"printf 'SURVIVED\\n'\r");
    lab.wait_for(&second, &mut second_cursor, b"SURVIVED");
}
