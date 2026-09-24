//! Private, atomic tab restore records. They contain identity, never commands
//! to rerun. The remote incarnation must still exist when the tab is restored.
#![allow(clippy::disallowed_types)]
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use warpui::AppContext;

use super::{connection_owner::ConnectionOwner, ssh_recipe::SshRecipe, terminal_manager::WorkspaceAttachment};
use crate::terminal::shell::ShellType;

const MAX_BYTES: u64 = 96 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceBookmark {
    version: u32,
    pub workspace_id: String,
    pub generation: String,
    shell: String,
    recipe: SshRecipe,
}

fn valid_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn root() -> Result<PathBuf> {
    Ok(dirs::home_dir().ok_or_else(|| anyhow!("Local home directory unavailable"))?
        .join(".local/state/eternalwarp/workspace-tabs"))
}

fn filename(uuid: &[u8]) -> Result<String> {
    Ok(format!("{}.json", uuid::Uuid::from_slice(uuid)?.simple()))
}

fn private_directory(root: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
        bail!("Workspace restore directory is not private to the local user");
    }
    Ok(())
}

impl WorkspaceBookmark {
    pub fn from_attachment(attachment: &WorkspaceAttachment, ctx: &AppContext) -> Result<Self> {
        let shell = match attachment.shell_type {
            ShellType::Bash => "bash", ShellType::Zsh => "zsh", ShellType::Fish => "fish",
            _ => bail!("Unsupported persistent workspace shell"),
        };
        let record = Self { version: 1, workspace_id: attachment.workspace_id.clone(),
            generation: attachment.generation.clone(), shell: shell.into(),
            recipe: attachment.owner.as_ref(ctx).recipe().clone() };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<()> {
        if self.version != 1 || !valid_id(&self.workspace_id) || !valid_id(&self.generation)
            || !matches!(self.shell.as_str(), "bash" | "zsh" | "fish") {
            bail!("Invalid persistent workspace restore record");
        }
        self.recipe.validate()
    }

    pub fn attach(&self, ctx: &mut AppContext) -> Result<WorkspaceAttachment> {
        self.validate()?;
        let shell_type = match self.shell.as_str() {
            "bash" => ShellType::Bash, "zsh" => ShellType::Zsh, "fish" => ShellType::Fish,
            _ => bail!("Unsupported persistent workspace shell"),
        };
        let owner = ConnectionOwner::create(self.recipe.clone(), ctx)?;
        Ok(WorkspaceAttachment { workspace_id: self.workspace_id.clone(), generation: self.generation.clone(),
            shell_type, control_path: owner.as_ref(ctx).socket_path(), connection: owner.as_ref(ctx).slot(), owner })
    }

    pub fn save(&self, uuid: &[u8]) -> Result<()> { self.save_at(&root()?, uuid) }

    fn save_at(&self, root: &Path, uuid: &[u8]) -> Result<()> {
        self.validate()?;
        let name = filename(uuid)?;
        DirBuilder::new().recursive(true).mode(0o700).create(root)?;
        private_directory(root)?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_BYTES as usize { bail!("Workspace restore record is too large"); }
        let mut file = tempfile::NamedTempFile::new_in(root)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(root.join(name))?;
        File::open(root)?.sync_all()?;
        Ok(())
    }

    pub fn load(uuid: &[u8]) -> Result<Option<Self>> { Self::load_at(&root()?, uuid) }

    fn load_at(root: &Path, uuid: &[u8]) -> Result<Option<Self>> {
        let name = filename(uuid)?;
        match std::fs::symlink_metadata(root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(_) => private_directory(root)?,
        }
        let file = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(root.join(name)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0 || metadata.len() > MAX_BYTES {
            bail!("Workspace restore record is not a private, bounded local file");
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_BYTES as usize { bail!("Workspace restore record is too large"); }
        let record: Self = serde_json::from_slice(&bytes)?;
        record.validate()?;
        Ok(Some(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn private_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    fn record() -> WorkspaceBookmark {
        serde_json::from_value(serde_json::json!({
            "version": 1, "workspace_id": "a".repeat(32), "generation": "b".repeat(32), "shell": "bash",
            "recipe": {"working_directory": "/home/test", "arguments": ["-F", "config with spaces", "lab"]}
        })).unwrap()
    }

    #[test]
    fn persistent_bookmark_round_trip_preserves_the_exact_incarnation() {
        let root = private_root();
        let id = uuid::Uuid::new_v4();
        let record = record();
        record.save_at(root.path(), id.as_bytes()).unwrap();
        assert_eq!(WorkspaceBookmark::load_at(root.path(), id.as_bytes()).unwrap(), Some(record));
        assert!(WorkspaceBookmark::load_at(root.path(), uuid::Uuid::new_v4().as_bytes()).unwrap().is_none());
    }

    #[test]
    fn persistent_bookmark_atomic_replacement_keeps_only_the_last_selection() {
        let root = private_root();
        let id = uuid::Uuid::new_v4();
        let mut record = record();
        record.save_at(root.path(), id.as_bytes()).unwrap();
        record.generation = "c".repeat(32);
        record.save_at(root.path(), id.as_bytes()).unwrap();
        assert_eq!(WorkspaceBookmark::load_at(root.path(), id.as_bytes()).unwrap(), Some(record));
    }

    #[test]
    fn persistent_bookmark_rejects_symlinks_and_public_files() {
        let root = private_root();
        let id = uuid::Uuid::new_v4();
        record().save_at(root.path(), id.as_bytes()).unwrap();
        let path = root.path().join(filename(id.as_bytes()).unwrap());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(WorkspaceBookmark::load_at(root.path(), id.as_bytes()).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let other = uuid::Uuid::new_v4();
        symlink(&path, root.path().join(filename(other.as_bytes()).unwrap())).unwrap();
        assert!(WorkspaceBookmark::load_at(root.path(), other.as_bytes()).is_err());
    }

    #[test]
    fn persistent_bookmark_rejects_invalid_identity_shell_and_version() {
        let mut record = record();
        record.version = 2;
        assert!(record.validate().is_err());
        record.version = 1;
        record.generation = "../another-workspace".into();
        assert!(record.validate().is_err());
        record.generation = "a".repeat(32);
        record.shell = "bash -c unexpected".into();
        assert!(record.validate().is_err());
        assert!(filename(b"invalid-uuid").is_err());
    }
}
