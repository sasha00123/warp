//! Trusted local SSH arguments, captured before the wrapper connects.
//! Never construct reconnect commands from remote escape-sequence payloads.
#![allow(clippy::disallowed_types)]
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

const MAX_RECIPE_BYTES: u64 = 64 * 1024;
const MAX_ARGUMENTS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshRecipe {
    working_directory: PathBuf,
    arguments: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedArguments {
    options: Vec<(char, Option<String>)>,
    destination: String,
}

fn parse_arguments(arguments: &[String]) -> Result<ParsedArguments> {
    if arguments.is_empty() || arguments.len() > MAX_ARGUMENTS {
        bail!("Invalid SSH argument count");
    }
    let mut options = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" {
            index += 1;
            break;
        }
        if !argument.starts_with('-') || argument == "-" {
            break;
        }
        for (offset, flag) in argument[1..].char_indices() {
            if "OGQVWwf".contains(flag) {
                bail!("SSH -{flag} is not supported for a persistent interactive connection");
            }
            if "BbcDEeFIiJLlmopRS".contains(flag) {
                let value_offset = 1 + offset + flag.len_utf8();
                let value = if value_offset < argument.len() {
                    argument[value_offset..].to_owned()
                } else {
                    index += 1;
                    arguments
                        .get(index)
                        .cloned()
                        .ok_or_else(|| anyhow!("SSH -{flag} needs an argument"))?
                };
                if value.is_empty() {
                    bail!("SSH -{flag} has an empty argument");
                }
                options.push((flag, Some(value)));
                break;
            }
            if !"1246AaCKkMNnqsTtXxYyvg".contains(flag) {
                bail!("Unsupported SSH option -{flag}");
            }
            options.push((flag, None));
        }
        index += 1;
    }
    if index + 1 != arguments.len() {
        bail!("Expected exactly one SSH destination and no remote command");
    }
    let destination = arguments[index].clone();
    if destination.is_empty() || destination.starts_with('-') || destination.contains('\0') {
        bail!("Invalid SSH destination");
    }
    Ok(ParsedArguments {
        options,
        destination,
    })
}

impl SshRecipe {
    pub fn load_local(session_id: warp_core::SessionId) -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Local home directory unavailable"))?;
        let root = home.join(".local/state/eternalwarp/ssh-recipes");
        Self::load_private_file(&root, &format!("{}.argv", session_id.as_u64()))
    }

    fn load_private_file(root: &Path, name: &str) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(root)?;
        // Files and the containing directory are local-user-owned, not writable
        // by other users and not symlinks supplied through a remote hook.
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            bail!("SSH reconnect directory is not private to the local user");
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join(name))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o077 != 0
            || metadata.len() > MAX_RECIPE_BYTES
        {
            bail!("SSH reconnect record is not a private, bounded local file");
        }
        let mut bytes = Vec::new();
        file.take(MAX_RECIPE_BYTES + 1).read_to_end(&mut bytes)?;
        Self::decode(&bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_RECIPE_BYTES as usize || bytes.last() != Some(&0) {
            bail!("Incomplete or oversized SSH reconnect record");
        }
        let fields = bytes[..bytes.len() - 1]
            .split(|byte| *byte == 0)
            .map(std::str::from_utf8)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if fields.len() < 3 || fields[0] != "EWSSH1" {
            bail!("Unknown SSH reconnect record format");
        }
        let recipe = Self {
            working_directory: PathBuf::from(fields[1]),
            arguments: fields[2..]
                .iter()
                .map(|field| (*field).to_owned())
                .collect(),
        };
        recipe.validate()?;
        Ok(recipe)
    }

    pub fn validate(&self) -> Result<()> {
        if !self.working_directory.is_absolute()
            || self.arguments.iter().any(|arg| arg.contains('\0'))
            || self.arguments.iter().map(String::len).sum::<usize>() > MAX_RECIPE_BYTES as usize
        {
            bail!("Invalid SSH reconnect recipe");
        }
        parse_arguments(&self.arguments)?;
        Ok(())
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    /// Explicit fallback when a complete native replay is no longer possible.
    /// This is executed only in a new local terminal, never in the remote job.
    pub fn recovery_command(&self, id: &str, generation: &str) -> Result<String> {
        self.validate()?;
        for token in [id, generation] {
            if token.len() != 32
                || !token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                bail!("Invalid recovery workspace identity");
            }
        }
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        let parsed = parse_arguments(&self.arguments)?;
        let mut args = vec!["-tt".to_owned(), "-S".into(), "none".into()];
        for setting in [
            "ControlMaster=no",
            "ControlPersist=no",
            "ControlPath=none",
            "StrictHostKeyChecking=yes",
            "ClearAllForwardings=yes",
            "RequestTTY=force",
            "SessionType=default",
            "RemoteCommand=none",
            "ServerAliveInterval=10",
            "ServerAliveCountMax=2",
        ] {
            args.extend(["-o".into(), setting.into()]);
        }
        for (flag, value) in parsed.options {
            if "SMNnTtDLR".contains(flag) {
                continue;
            }
            if flag == 'o'
                && let Some(value) = &value
            {
                let key = value.split(['=', ' ', '\t']).next().unwrap_or_default();
                if [
                    "ControlMaster",
                    "ControlPath",
                    "ControlPersist",
                    "RequestTTY",
                    "SessionType",
                    "LocalForward",
                    "RemoteForward",
                    "DynamicForward",
                    "ForkAfterAuthentication",
                    "RemoteCommand",
                    "StdinNull",
                ]
                .iter()
                .any(|item| key.eq_ignore_ascii_case(item))
                {
                    continue;
                }
            }
            args.push(format!("-{flag}"));
            if let Some(value) = value {
                args.push(value);
            }
        }
        let session = format!("ew-{id}");
        let target = format!("={session}:0.0");
        let guard = format!("#{{==:#{{WARP_WORKSPACE_GENERATION}},{generation}}}");
        // Older tmux accepts '=' for pane/window targets, but not the session
        // target of set-option. The incarnation guard still runs before attach.
        let attach = format!(
            "set-option -t {session} status off ; set-option -w -t ={session}:0 window-size latest ; attach-session -t {session}"
        );
        let remote = [
            "tmux",
            "-L",
            remote_server::persistent_workspace::SOCKET,
            "if-shell",
            "-F",
            "-t",
            &target,
            &guard,
            &attach,
            "display-message -p 'Workspace was removed or replaced; nothing was started'",
        ]
        .into_iter()
        .map(quote)
        .collect::<Vec<_>>()
        .join(" ");
        args.extend(["--".into(), parsed.destination, remote]);
        let directory = self
            .working_directory
            .to_str()
            .ok_or_else(|| anyhow!("SSH working directory is not UTF-8"))?;
        Ok(format!(
            "cd -- {} && /usr/bin/ssh {}",
            quote(directory),
            args.iter()
                .map(|arg| quote(arg))
                .collect::<Vec<_>>()
                .join(" ")
        ))
    }

    /// Start a foreground, private, non-interactive master. Authentication
    /// failures stay visible; this never accepts a new or changed host key.
    /// No forwarding/listening ports from the interactive command are copied.
    pub fn master_arguments(&self, socket: &Path) -> Result<Vec<String>> {
        self.validate()?;
        let socket = socket
            .to_str()
            .filter(|_| socket.is_absolute())
            .ok_or_else(|| anyhow!("SSH control socket must be an absolute UTF-8 path"))?;
        let parsed = parse_arguments(&self.arguments)?;
        let mut args = vec!["-N".into(), "-T".into()];
        for setting in [
            "ControlMaster=yes",
            "ControlPersist=no",
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "ServerAliveInterval=10",
            "ServerAliveCountMax=2",
            "ClearAllForwardings=yes",
            "RequestTTY=no",
            "SessionType=none",
            "ExitOnForwardFailure=yes",
        ] {
            args.extend(["-o".into(), setting.into()]);
        }
        args.extend(["-S".into(), socket.into()]);
        for (flag, value) in parsed.options {
            // -S and -t override their -o counterparts regardless of order;
            // remove them instead of relying on OpenSSH's first-value rule.
            if "SMNTtDLR".contains(flag) {
                continue;
            }
            if flag == 'o'
                && let Some(value) = &value
            {
                let key = value.split(['=', ' ', '\t']).next().unwrap_or_default();
                if [
                    "ControlMaster",
                    "ControlPath",
                    "ControlPersist",
                    "RequestTTY",
                    "SessionType",
                    "LocalForward",
                    "RemoteForward",
                    "DynamicForward",
                    "ForkAfterAuthentication",
                    "RemoteCommand",
                ]
                .iter()
                .any(|item| key.eq_ignore_ascii_case(item))
                {
                    continue;
                }
            }
            args.push(format!("-{flag}"));
            if let Some(value) = value {
                args.push(value);
            }
        }
        args.extend(["--".into(), parsed.destination]);
        Ok(args)
    }
}

#[cfg(test)]
#[path = "ssh_recipe_tests.rs"]
mod tests;
