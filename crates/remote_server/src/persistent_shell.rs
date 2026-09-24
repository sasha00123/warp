//! A tmux-owned, recorder-gated shell launcher. Reattachment never runs this.
#![allow(clippy::disallowed_types)]
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const SHELL_ARGUMENT: &str = "--persistent-workspace-shell";

pub fn session_id(generation: &str) -> Option<u64> {
    if generation.len() != 32
        || !generation.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    { return None; }
    Some(u64::from_str_radix(&generation[..16], 16).ok()?.max(1))
}

pub fn supported_shell(shell: &str) -> bool {
    matches!(shell, "bash" | "zsh" | "fish")
}

pub fn find_shell(shell: &str) -> io::Result<PathBuf> {
    if !supported_shell(shell) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Unsupported persistent shell"));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for directory in std::env::split_paths(&path).chain([PathBuf::from("/bin"), PathBuf::from("/usr/bin")]) {
        let candidate = directory.join(shell);
        if candidate.is_absolute() && candidate.is_file() {
            return candidate.canonicalize();
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("Remote shell {shell} is not installed")))
}

pub fn run_shell_if_requested() -> Option<io::Result<()>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(SHELL_ARGUMENT)) { return None; }
    Some((|| {
        let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "Invalid persistent shell launch");
        let directory = PathBuf::from(args.next().ok_or_else(invalid)?);
        let shell = args.next().and_then(|value| value.into_string().ok()).ok_or_else(invalid)?;
        let executable = PathBuf::from(args.next().ok_or_else(invalid)?);
        if args.next().is_some() || !directory.is_absolute() || !executable.is_absolute()
            || !supported_shell(&shell) { return Err(invalid()); }
        // Only a new pane waits here. A lost create response is safe to retry.
        while !directory.join("ready").is_file() {
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut command = Command::new(executable);
        command.env("TERM_PROGRAM", "WarpTerminal")
            .env("WARP_PERSISTENT_WORKSPACE", "1")
            .env_remove("WARP_BOOTSTRAPPED").env_remove("WARP_SESSION_ID")
            .env_remove("BASH_ENV").env_remove("ENV");
        match shell.as_str() {
            "bash" => {
                command.args(["--noprofile", "--rcfile"]).arg(directory.join("startup")).arg("-i");
            }
            "zsh" => {
                if let Some(original) = std::env::var_os("ZDOTDIR") {
                    command.env("WARP_PERSISTENT_ORIGINAL_ZDOTDIR", original)
                        .env("WARP_PERSISTENT_HAD_ZDOTDIR", "1");
                } else {
                    command.env_remove("WARP_PERSISTENT_ORIGINAL_ZDOTDIR")
                        .env_remove("WARP_PERSISTENT_HAD_ZDOTDIR");
                }
                command.env("ZDOTDIR", &directory).arg("-i");
            }
            "fish" => {
                let startup = directory.join("startup");
                let startup = startup.to_str().ok_or_else(invalid)?;
                command.args(["--no-config", "--interactive", "--init-command"])
                    .arg(format!("source '{}'", startup.replace('\'', "'\\''")));
            }
            _ => return Err(invalid()),
        }
        Err(command.exec())
    })())
}

pub(crate) fn startup_path(root: &Path, id: &str, generation: &str) -> PathBuf {
    root.join(format!("{id}-{generation}.shell"))
}

pub(crate) fn stored_shell(id: &str, generation: &str) -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let root = PathBuf::from(home).join(".local/state/warp/persistent-output-v1");
    let shell = std::fs::read_to_string(startup_path(&root, id, generation).join("shell")).ok()?;
    supported_shell(&shell).then_some(shell)
}

/// Execution state written by the root shell's hooks. Never infer idle from
/// the shell PID or executable: shell builtins can run indefinitely too.
pub fn activity(root: &Path, id: &str, generation: &str) -> Option<&'static str> {
    use std::io::Read;
    session_id(id)?;
    session_id(generation)?;
    let mut bytes = Vec::new();
    std::fs::File::open(startup_path(root, id, generation).join("activity"))
        .ok()?.take(16).read_to_end(&mut bytes).ok()?;
    match bytes.as_slice() {
        b"starting\n" => Some("starting"),
        b"running\n" => Some("running"),
        b"idle\n" => Some("idle"),
        _ => None,
    }
}
