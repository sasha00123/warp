use super::*;
use crate::persistent_shell;
use std::fs::{self, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

pub struct BootstrapProfile<'a> {
    pub shell: &'a str,
    /// Canonical Warp init script, retaining @@WARP_SESSION_ID@@.
    pub init_script: &'a [u8],
    /// Canonical fully expanded Warp bootstrap script for this shell.
    pub bootstrap_script: &'a [u8],
}

fn failure(error: std::io::Error) -> Error {
    Error::new(
        ErrorKind::Failed,
        format!("Persistent shell startup: {error}"),
    )
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(failure)?;
    file.write_all(bytes).map_err(failure)?;
    file.sync_all().map_err(failure)
}

impl Backend {
    /// The remote worker records and releases the shell independently of SSH.
    pub fn create_initialized(
        &self,
        id: &str,
        directory: Option<&Path>,
        profile: BootstrapProfile<'_>,
        executable: &Path,
        root: &Path,
    ) -> Result<Workspace, Error> {
        self.create_initialized_with_retention(
            id,
            directory,
            profile,
            executable,
            root,
            crate::persistent_journal::RetentionPolicy::default(),
        )
    }

    pub fn create_initialized_with_retention(
        &self,
        id: &str,
        directory: Option<&Path>,
        profile: BootstrapProfile<'_>,
        executable: &Path,
        root: &Path,
        retention: crate::persistent_journal::RetentionPolicy,
    ) -> Result<Workspace, Error> {
        validate_token(id)?;
        if !persistent_shell::supported_shell(profile.shell)
            || profile.init_script.is_empty()
            || profile.init_script.len() > 64 * 1024
            || profile.bootstrap_script.is_empty()
            || profile.bootstrap_script.len() > 1024 * 1024
            || !root.is_absolute()
            || root.to_str().is_none()
            || !executable.is_absolute()
            || executable.to_str().is_none()
        {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "Invalid persistent shell profile",
            ));
        }
        let init = std::str::from_utf8(profile.init_script)
            .map_err(|_| Error::new(ErrorKind::InvalidRequest, "Shell init must be UTF-8"))?;
        if !init.contains("@@WARP_SESSION_ID@@") {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "Shell init is missing its session token placeholder",
            ));
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .map_err(failure)?;
        let metadata = fs::symlink_metadata(root).map_err(failure)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::new(
                ErrorKind::Failed,
                "Shell state root must be private (0700)",
            ));
        }
        // Per-ID advisory locks are released even if the daemon crashes.
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(root.join(format!("{id}.lock")))
            .map_err(failure)?;
        let deadline = Instant::now() + self.timeout;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(Error::new(
                        ErrorKind::Timeout,
                        "Workspace initialization is still in progress",
                    ));
                }
                Err(TryLockError::Error(error)) => return Err(failure(error)),
            }
        }
        match self.lookup(id) {
            Ok(workspace) => {
                let startup = persistent_shell::startup_path(root, id, &workspace.generation);
                if !startup.join("startup").is_file() {
                    return Err(Error::new(
                        ErrorKind::Failed,
                        "Existing workspace was not created with durable shell integration",
                    ));
                }
                self.finish_startup(&workspace, executable, root, &startup)?;
                return Ok(workspace);
            }
            Err(error) if error.kind == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let generation = random_token()?;
        let startup = persistent_shell::startup_path(root, id, &generation);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&startup)
            .map_err(failure)?;
        let session_id = persistent_shell::session_id(&generation).unwrap();
        let init = init
            .replace("@@WARP_SESSION_ID@@", &session_id.to_string())
            .replace("@@USING_CON_PTY_BOOLEAN@@", "false");
        let activity_path = quote(startup.join("activity").to_str().unwrap());
        let prefix = if profile.shell == "fish" {
            format!(
                "set -gx WARP_PERSISTENT_ACTIVITY_PATH {activity_path}\nset -gx WARP_PERSISTENT_ROOT_SESSION {session_id}\n"
            )
        } else {
            format!(
                "export WARP_PERSISTENT_ACTIVITY_PATH={activity_path}\nexport WARP_PERSISTENT_ROOT_SESSION={session_id}\n"
            )
        };
        let mut script = prefix.into_bytes();
        script.extend_from_slice(init.as_bytes());
        script.push(b'\n');
        script.extend_from_slice(profile.bootstrap_script);
        script.push(b'\n');
        private_write(&startup.join("startup"), &script)?;
        private_write(&startup.join("shell"), profile.shell.as_bytes())?;
        private_write(&startup.join("activity"), b"starting\n")?;
        private_write(
            &startup.join("retention"),
            match retention {
                crate::persistent_journal::RetentionPolicy::UntilWorkspaceDeleted => b"archive\n",
                crate::persistent_journal::RetentionPolicy::Rolling => b"rolling\n",
            },
        )?;
        if profile.shell == "zsh" {
            let mut zsh = b"if [[ -n $WARP_PERSISTENT_HAD_ZDOTDIR ]]; then ZDOTDIR=$WARP_PERSISTENT_ORIGINAL_ZDOTDIR; else unset ZDOTDIR; fi\nunset WARP_PERSISTENT_HAD_ZDOTDIR WARP_PERSISTENT_ORIGINAL_ZDOTDIR\n".to_vec();
            zsh.extend_from_slice(&script);
            private_write(&startup.join(".zshrc"), &zsh)?;
        }
        let shell = persistent_shell::find_shell(profile.shell).map_err(failure)?;
        let shell = shell
            .to_str()
            .ok_or_else(|| Error::new(ErrorKind::Failed, "Shell path is not UTF-8"))?;
        let command = format!(
            "exec {} {} {} {} {}",
            quote(executable.to_str().unwrap()),
            persistent_shell::SHELL_ARGUMENT,
            quote(startup.to_str().unwrap()),
            quote(profile.shell),
            quote(shell)
        );
        let workspace =
            self.create_with_startup(id, directory, Some(&generation), Some(&command))?;
        self.finish_startup(&workspace, executable, root, &startup)?;
        Ok(workspace)
    }

    fn finish_startup(
        &self,
        workspace: &Workspace,
        executable: &Path,
        root: &Path,
        startup: &Path,
    ) -> Result<(), Error> {
        // Never rerun initialization after release, even if a job is now running.
        if startup.join("ready").is_file() {
            return Ok(());
        }
        let retention = match fs::read(startup.join("retention")) {
            Ok(value) if value == b"rolling\n" => {
                crate::persistent_journal::RetentionPolicy::Rolling
            }
            Ok(value) if value == b"archive\n" => {
                crate::persistent_journal::RetentionPolicy::UntilWorkspaceDeleted
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::persistent_journal::RetentionPolicy::default()
            }
            Ok(_) => {
                return Err(Error::new(
                    ErrorKind::Failed,
                    "Invalid saved history retention policy",
                ));
            }
            Err(error) => return Err(failure(error)),
        };
        self.start_recording_with_retention(
            &workspace.id,
            &workspace.generation,
            executable,
            root,
            retention,
        )?;
        private_write(&startup.join("ready"), b"ready\n")?;
        File::open(startup)
            .and_then(|file| file.sync_all())
            .map_err(failure)
    }
}
