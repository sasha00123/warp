//! No user-shell injection: tmux commands are sent by the extension itself.

use super::*;
use crate::persistent_journal::{self, OutputPage};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn io_error(error: std::io::Error) -> Error {
    Error::new(ErrorKind::Failed, format!("Workspace journal: {error}"))
}

impl Backend {
    /// Start exactly one recorder owned by tmux, not by an SSH connection.
    /// Calling this again only observes the existing recorder, never replaces it.
    pub fn start_recording(
        &self,
        id: &str,
        generation: &str,
        executable: &Path,
        root: &Path,
    ) -> Result<(), Error> {
        self.start_recording_with_retention(id, generation, executable, root,
            persistent_journal::RetentionPolicy::default())
    }

    pub fn start_recording_with_retention(
        &self, id: &str, generation: &str, executable: &Path, root: &Path,
        retention: persistent_journal::RetentionPolicy,
    ) -> Result<(), Error> {
        let workspace = self.resolve(id, generation)?;
        let path = self.journal_path(id, generation, root)?;
        // The dedicated state root must not expose recorded terminal secrets to
        // other users, nor follow a pre-existing symlink.
        match fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
        {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error)),
        }
        let metadata = fs::symlink_metadata(root).map_err(io_error)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::new(
                ErrorKind::Failed,
                "Journal root must be a private directory (mode 0700)",
            ));
        }
        if path.exists() {
            persistent_journal::read(&path, 0, 1).map_err(io_error)?;
            if !workspace.exited && !self.recorder_connected(id, generation)? {
                return Err(Error::new(
                    ErrorKind::Failed,
                    "Recorder disconnected; refusing to conceal an output gap",
                ));
            }
            return Ok(());
        }
        // Reattachment never launches a recorder. An extension executable may
        // have been atomically replaced while this daemon and recorder stayed
        // alive; on Linux current_exe then names a deleted inode. Only a new
        // recorder needs an executable that can still be launched by path.
        if !executable.is_absolute() || executable.to_str().is_none() || !executable.is_file() {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "Recorder executable must be an existing absolute UTF-8 path",
            ));
        }
        // pipe-pane expands tmux formats even inside shell quotes. Escape '#'
        // before quoting the command so filesystem names remain literal data.
        let retention_argument = match retention {
            persistent_journal::RetentionPolicy::UntilWorkspaceDeleted => "",
            persistent_journal::RetentionPolicy::Rolling => " --bounded-history",
        };
        let command = format!(
            "exec {} {} {}{retention_argument}",
            quote(executable.to_str().unwrap()),
            persistent_journal::RECORDER_ARGUMENT,
            quote(path.to_str().unwrap())
        )
        .replace('#', "##");
        self.guarded(
            id,
            generation,
            &format!(
                "set-option -w -t {} remain-on-exit on ; pipe-pane -O -o -t {} {}",
                workspace.window_id,
                workspace.pane_id,
                quote(&command),
            ),
        )?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match persistent_journal::read(&path, 0, 1) {
                Ok(_) => return Ok(()),
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(io_error(error)),
            }
        }
    }

    pub fn delete_retained_history(&self, id: &str, generation: &str, root: &Path) -> Result<(), Error> {
        let journal = self.journal_path(id, generation, root)?;
        match self.resolve(id, generation) {
            Ok(_) => return Err(Error::new(ErrorKind::Failed, "Terminate the workspace before deleting its history")),
            Err(error) if matches!(error.kind, ErrorKind::NotFound | ErrorKind::Stale) => {},
            Err(error) => return Err(error),
        }
        match fs::symlink_metadata(root) {
            Ok(metadata) if metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0 => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(_) => return Err(Error::new(ErrorKind::Failed, "History root must be a private directory")),
            Err(error) => return Err(io_error(error)),
        }
        for path in [journal, crate::persistent_shell::startup_path(root, id, generation)] {
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&path).map_err(io_error)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Ok(_) => return Err(Error::new(ErrorKind::Failed, "Refusing to follow a history symlink")),
                Err(error) => return Err(io_error(error)),
            }
        }
        Ok(())
    }

    pub fn read_output(
        &self,
        id: &str,
        generation: &str,
        root: &Path,
        cursor: u64,
    ) -> Result<OutputPage, Error> {
        let workspace = self.resolve(id, generation)?;
        let path = self.journal_path(id, generation, root)?;
        let read = || persistent_journal::read(&path, cursor, persistent_journal::MAX_PAGE_BYTES)
            .map_err(io_error);
        let mut page = read()?;
        if workspace.exited && !page.closed {
            persistent_journal::request_finalization(&path).map_err(io_error)?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while !page.closed {
                if Instant::now() >= deadline {
                    return Err(Error::new(ErrorKind::Failed,
                        "Shell exited, but its output recorder did not finalize; output may be incomplete"));
                }
                thread::sleep(Duration::from_millis(10));
                page = read()?;
            }
        }
        Ok(page)
    }

    pub fn recorder_connected(&self, id: &str, generation: &str) -> Result<bool, Error> {
        let workspace = self.resolve(id, generation)?;
        let result = self.run(&[
            "display-message",
            "-p",
            "-t",
            &workspace.pane_id,
            "#{pane_pipe}",
        ])?;
        Ok(result.trim() == "1")
    }

    /// Never automatically retry an input request: a lost acknowledgement does
    /// not imply that the keystrokes were not delivered. Ctrl-C is the byte 0x03.
    pub fn send_input(&self, id: &str, generation: &str, bytes: &[u8]) -> Result<(), Error> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "Input must contain 1..4096 bytes",
            ));
        }
        let workspace = self.resolve(id, generation)?;
        if workspace.exited {
            return Err(Error::new(
                ErrorKind::Failed,
                "The workspace shell has exited",
            ));
        }
        let hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        self.guarded(
            id,
            generation,
            &format!("send-keys -H -t {} {hex}", workspace.pane_id),
        )
    }

    pub fn resize(&self, id: &str, generation: &str, columns: u32, rows: u32) -> Result<(), Error> {
        if !(2..=1000).contains(&columns) || !(2..=1000).contains(&rows) {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "Terminal dimensions must be in 2..1000",
            ));
        }
        let workspace = self.resolve(id, generation)?;
        self.guarded(id, generation, &format!(
            "set-option -w -t {0} window-size manual ; resize-window -t {0} -x {columns} -y {rows}",
            workspace.window_id,
        ))
    }

    fn journal_path(&self, id: &str, generation: &str, root: &Path) -> Result<PathBuf, Error> {
        validate_token(id)?;
        validate_token(generation)?;
        if !root.is_absolute() || root.to_str().is_none() {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "Journal root must be an absolute UTF-8 path",
            ));
        }
        Ok(root.join(format!("{id}-{generation}")))
    }

    fn guarded(&self, id: &str, generation: &str, command: &str) -> Result<(), Error> {
        validate_token(id)?;
        validate_token(generation)?;
        let target = format!("={PREFIX}{id}:0.0");
        let guard = format!("#{{==:#{{WARP_WORKSPACE_GENERATION}},{generation}}}");
        let response = self.run(&[
            "if-shell",
            "-F",
            "-t",
            &target,
            &guard,
            command,
            "display-message -p STALE_WORKSPACE",
        ])?;
        if response.contains("STALE_WORKSPACE") {
            return Err(Error::new(
                ErrorKind::Stale,
                "Workspace incarnation changed before terminal operation",
            ));
        }
        Ok(())
    }
}
