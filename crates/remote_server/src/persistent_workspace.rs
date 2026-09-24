//! Managed tmux workspaces, independent of SSH and extension daemon lifetimes.

// The VM harness also compiles this Unix-only module directly, without workspace dependencies.
#![allow(clippy::disallowed_types)]

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PREFIX: &str = "ew-";
#[path = "persistent_workspace_startup.rs"]
mod startup;
#[path = "persistent_workspace_transport.rs"]
mod transport;
pub use startup::BootstrapProfile;
const MAX_OUTPUT: u64 = 1024 * 1024;
const FORMAT: &str = "#{session_name}|#{WARP_WORKSPACE_GENERATION}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}";
pub const SOCKET: &str = "eternalwarp-workspaces-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidRequest,
    Unavailable,
    NotFound,
    Stale,
    Failed,
    Timeout,
}

impl ErrorKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Unavailable => "tmux_unavailable",
            Self::NotFound => "workspace_not_found",
            Self::Stale => "stale_workspace",
            Self::Failed => "tmux_failed",
            Self::Timeout => "tmux_timeout",
        }
    }
}

#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind.code(), self.message)
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub id: String,
    pub generation: String,
    pub session_id: String,
    pub window_id: String,
    pub pane_id: String,
    pub shell_pid: u32,
    pub exited: bool,
    pub exit_code: Option<i32>,
}

#[derive(Clone)]
pub struct Backend {
    socket: String,
    executable: PathBuf,
    timeout: Duration,
}

impl Backend {
    pub fn new(socket: &str) -> Result<Self, Error> {
        if socket.is_empty()
            || socket.len() > 80
            || !socket
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "Invalid managed socket name",
            ));
        }
        Ok(Self {
            socket: socket.to_owned(),
            executable: PathBuf::from("tmux"),
            timeout: Duration::from_secs(5),
        })
    }

    pub fn probe(&self) -> Result<(), Error> {
        let version = self.run(&["-V"])?;
        let mut parts = version
            .trim()
            .strip_prefix("tmux ")
            .unwrap_or("")
            .split('.');
        let major = parts
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let minor = parts
            .next()
            .map(|s| {
                s.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
            })
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        if (major, minor) < (3, 2) {
            return Err(Error::new(
                ErrorKind::Unavailable,
                "Persistent workspaces require tmux 3.2 or newer",
            ));
        }
        Ok(())
    }

    pub fn create(&self, id: &str, directory: Option<&Path>) -> Result<Workspace, Error> {
        self.create_with_startup(id, directory, None, None)
    }

    fn create_with_startup(
        &self,
        id: &str,
        directory: Option<&Path>,
        generation: Option<&str>,
        command: Option<&str>,
    ) -> Result<Workspace, Error> {
        validate_token(id)?;
        if directory
            .is_some_and(|path| !path.is_absolute() || !path.is_dir() || path.to_str().is_none())
        {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "Working directory must be an existing absolute UTF-8 path",
            ));
        }
        match self.lookup(id) {
            Ok(workspace) => return Ok(workspace),
            Err(error) if error.kind == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.probe()?;
        let name = format!("{PREFIX}{id}");
        let generation = match generation {
            Some(generation) => {
                validate_token(generation)?;
                generation.to_owned()
            }
            None => random_token()?,
        };
        let generation = format!("WARP_WORKSPACE_GENERATION={generation}");
        let mut args = vec![
            "new-session",
            "-d",
            "-s",
            &name,
            "-x",
            "120",
            "-y",
            "35",
            "-e",
            &generation,
        ];
        if let Some(path) = directory {
            args.extend(["-c", path.to_str().unwrap()]);
        }
        if let Some(command) = command {
            args.push(command);
        }
        // The environment and shell are created in one tmux operation. A retry can
        // never observe a workspace awaiting a second metadata-initialization command.
        match self.run(&args) {
            Ok(_) => self.lookup(id),
            Err(error) => match self.lookup(id) {
                Ok(workspace) => Ok(workspace),
                Err(_) => Err(error),
            },
        }
    }

    pub fn list(&self) -> Result<Vec<Workspace>, Error> {
        let names = match self.run(&["list-sessions", "-F", "#{session_name}"]) {
            Ok(names) => names,
            Err(error) if error.kind == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut result = Vec::new();
        for name in names.lines() {
            let Some(id) = name.strip_prefix(PREFIX) else {
                continue;
            };
            if validate_token(id).is_err() {
                continue;
            }
            match self.lookup(id) {
                Ok(workspace) => result.push(workspace),
                Err(error) if error.kind == ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            if result.len() > 1024 {
                return Err(Error::new(ErrorKind::Failed, "Too many managed workspaces"));
            }
        }
        result.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(result)
    }

    pub fn resolve(&self, id: &str, generation: &str) -> Result<Workspace, Error> {
        validate_token(id)?;
        validate_token(generation)?;
        let workspace = self.lookup(id)?;
        if workspace.generation != generation {
            return Err(Error::new(
                ErrorKind::Stale,
                "The workspace was replaced; refresh the workspace list",
            ));
        }
        Ok(workspace)
    }

    pub fn terminate(&self, id: &str, generation: &str) -> Result<(), Error> {
        let workspace = self.resolve(id, generation)?;
        let target = format!("={PREFIX}{id}:0.0");
        let guard = format!("#{{==:#{{WARP_WORKSPACE_GENERATION}},{generation}}}");
        let kill = format!("kill-session -t {}", workspace.session_id);
        // Recheck the incarnation on the same tmux connection that performs deletion.
        // A stale numeric ID must never target a replacement server's workspace.
        let output = self.run(&[
            "if-shell",
            "-F",
            "-t",
            &target,
            &guard,
            &kill,
            "display-message -p STALE_WORKSPACE",
        ])?;
        if output.contains("STALE_WORKSPACE") {
            return Err(Error::new(
                ErrorKind::Stale,
                "Workspace incarnation changed before deletion",
            ));
        }
        Ok(())
    }

    fn lookup(&self, id: &str) -> Result<Workspace, Error> {
        self.lookup_with_status_refresh(id, true)
    }

    fn lookup_with_status_refresh(
        &self,
        id: &str,
        refresh_status: bool,
    ) -> Result<Workspace, Error> {
        validate_token(id)?;
        let target = format!("={PREFIX}{id}:0.0");
        let output = self.run(&["display-message", "-p", "-t", &target, FORMAT])?;
        let fields: Vec<_> = output.trim_end_matches(['\r', '\n']).split('|').collect();
        if fields[0].is_empty() {
            return Err(Error::new(
                ErrorKind::NotFound,
                "Workspace no longer exists",
            ));
        }
        if fields.len() != 9
            || fields[0] != format!("{PREFIX}{id}")
            || validate_token(fields[1]).is_err()
            || !numeric_id(fields[2], '$')
            || !numeric_id(fields[3], '@')
            || !numeric_id(fields[4], '%')
        {
            return Err(Error::new(
                ErrorKind::Failed,
                "Workspace metadata is missing or malformed; refusing to adopt it",
            ));
        }
        let shell_pid = fields[5]
            .parse()
            .map_err(|_| Error::new(ErrorKind::Failed, "Invalid shell PID"))?;
        let exited = match fields[6] {
            "0" => false,
            "1" => true,
            _ => return Err(Error::new(ErrorKind::Failed, "Invalid pane state")),
        };
        // Older tmux/libutempter combinations can consume the child notification.
        // Ask this managed server to reap once, as newer tmux does internally.
        // The command is constant, out-of-band, and never targets the pane's job.
        if exited && fields[7].is_empty() && fields[8].is_empty() && refresh_status {
            self.run(&["run-shell", "-t", &target, "kill -CHLD #{pid}"])?;
            return self.lookup_with_status_refresh(id, false);
        }
        // A dead PTY can precede waitpid. Absence is unknown, not success.
        let exit_code = if exited && !fields[7].is_empty() {
            Some(
                fields[7]
                    .parse::<i32>()
                    .ok()
                    .filter(|code| (0..=255).contains(code))
                    .ok_or_else(|| Error::new(ErrorKind::Failed, "Invalid shell exit status"))?,
            )
        } else {
            None
        };
        Ok(Workspace {
            id: id.to_owned(),
            generation: fields[1].to_owned(),
            session_id: fields[2].to_owned(),
            window_id: fields[3].to_owned(),
            pane_id: fields[4].to_owned(),
            shell_pid,
            exited,
            exit_code,
        })
    }

    fn run(&self, args: &[&str]) -> Result<String, Error> {
        // Socket pairs provide nonblocking reads without detached reader threads.
        // A daemon may inherit stdout; waiting for EOF after the client exits
        // would otherwise bypass the command deadline indefinitely.
        let io_error = |error: std::io::Error| Error::new(ErrorKind::Failed, error.to_string());
        let (mut stdout, child_stdout) = UnixStream::pair().map_err(io_error)?;
        let (mut stderr, child_stderr) = UnixStream::pair().map_err(io_error)?;
        stdout.set_nonblocking(true).map_err(io_error)?;
        stderr.set_nonblocking(true).map_err(io_error)?;
        let mut child = Command::new(&self.executable)
            .args(["-L", &self.socket, "-f", "/dev/null"])
            .args(args)
            .env_remove("TMUX")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::from(OwnedFd::from(child_stdout)))
            .stderr(Stdio::from(OwnedFd::from(child_stderr)))
            .spawn()
            .map_err(|error| {
                Error::new(
                    ErrorKind::Unavailable,
                    format!("Could not start tmux: {error}"),
                )
            })?;
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        let drain = |stream: &mut UnixStream, bytes: &mut Vec<u8>| -> Result<(), Error> {
            let mut buffer = [0; 4096];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => return Ok(()),
                    Ok(count) => {
                        if bytes.len() as u64 + count as u64 > MAX_OUTPUT {
                            return Err(Error::new(
                                ErrorKind::Failed,
                                "tmux output exceeded the safety limit",
                            ));
                        }
                        bytes.extend_from_slice(&buffer[..count]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(io_error(error)),
                }
            }
        };
        let deadline = Instant::now() + self.timeout;
        let result = (|| {
            loop {
                // Observe exit before draining so all bytes written by the
                // command itself are consumed, without waiting on descendants.
                let status = child.try_wait().map_err(io_error)?;
                drain(&mut stdout, &mut stdout_bytes)?;
                drain(&mut stderr, &mut stderr_bytes)?;
                if let Some(status) = status {
                    return Ok(status);
                }
                if Instant::now() >= deadline {
                    return Err(Error::new(
                        ErrorKind::Timeout,
                        "tmux did not respond within the command deadline",
                    ));
                }
                thread::sleep(Duration::from_millis(10));
            }
        })();
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let status = result?;
        let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        if !status.success() {
            let kind = if stderr.contains("can't find session")
                || stderr.contains("can't find pane")
                || stderr.contains("can't find window")
                || stderr.contains("no server running")
                || stderr.contains("No such file or directory")
            {
                ErrorKind::NotFound
            } else {
                ErrorKind::Failed
            };
            return Err(Error::new(kind, stderr.trim().to_owned()));
        }
        Ok(stdout)
    }
}

fn validate_token(value: &str) -> Result<(), Error> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "Workspace IDs and generations must be 32 lowercase hex characters",
        ));
    }
    Ok(())
}

fn numeric_id(value: &str, prefix: char) -> bool {
    value
        .strip_prefix(prefix)
        .map(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or(false)
}

fn random_token() -> Result<String, Error> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| {
            Error::new(
                ErrorKind::Failed,
                format!("Could not generate workspace identity: {error}"),
            )
        })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
#[path = "persistent_workspace_tests.rs"]
mod tests;
