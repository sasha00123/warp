//! Owns a connection independently of any interactive SSH tab.
//! Dropping it closes local transport only, never a remote workspace.
use std::path::{Path, PathBuf};
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use instant::Instant;

use anyhow::{Result, anyhow, bail};
use async_process::Child;
use command::r#async::Command;
use futures_lite::io::AsyncReadExt;
use parking_lot::Mutex;
use remote_server::manager::{RemoteServerManager, RemoteServerManagerEvent};
use warp_core::SessionId;
use warpui::{AppContext, Entity, ModelContext, ModelHandle, SingletonEntity};
use warpui::r#async::FutureExt as _;

use super::{connection::ConnectionSlot, ssh_recipe::SshRecipe};
use crate::auth::auth_state::AuthStateProvider;
use crate::remote_server::auth_context::server_api_auth_context;
use crate::remote_server::ssh_transport::SshTransport;
use crate::server::server_api::ServerApiProvider;
use crate::settings::PrivacySettings;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Reconnecting { attempt: u32, error: String },
}

pub struct ConnectionOwner {
    recipe: SshRecipe,
    directory: Arc<tempfile::TempDir>,
    slot: ConnectionSlot,
    master: Option<Child>,
    session_id: SessionId,
    lifetime: Option<async_channel::Sender<()>>,
    status: ConnectionStatus,
    retry_at: Option<Instant>,
    attempts: u32,
}

impl Entity for ConnectionOwner { type Event = (); }

fn retry_delay(attempt: u32) -> Duration {
    Duration::from_secs(1_u64.checked_shl(attempt.saturating_sub(1).min(5)).unwrap_or(30).min(30))
}

fn private_master_directory() -> Result<tempfile::TempDir> {
    let directory = tempfile::Builder::new().prefix("ew-ssh-").tempdir_in("/tmp")?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

impl ConnectionOwner {
    pub fn create(recipe: SshRecipe, ctx: &mut AppContext) -> Result<ModelHandle<Self>> {
        recipe.validate()?;
        // Keep the Unix socket path short even when HOME is a long path.
        let directory = Arc::new(private_master_directory()?);
        Ok(ctx.add_model(|ctx: &mut ModelContext<Self>| {
            let manager = RemoteServerManager::handle(ctx);
            ctx.subscribe_to_model(&manager, |me, _, event, ctx| {
                match event {
                    RemoteServerManagerEvent::SessionConnected { session_id, .. }
                        if *session_id == me.session_id => {
                        let client = RemoteServerManager::as_ref(ctx).client_for_session(*session_id).cloned();
                        if client.is_some() {
                            me.slot.replace(client);
                            me.status = ConnectionStatus::Connected;
                            me.attempts = 0;
                            me.retry_at = None;
                            ctx.emit(());
                        }
                    }
                    RemoteServerManagerEvent::SessionReconnected { session_id, client, .. }
                        if *session_id == me.session_id && me.retry_at.is_none() => {
                        me.slot.replace(Some(client.clone()));
                        me.status = ConnectionStatus::Connected;
                        me.attempts = 0;
                        ctx.emit(());
                    }
                    RemoteServerManagerEvent::SessionDisconnected { session_id, .. }
                        if *session_id == me.session_id => {
                        me.failed("SSH connection was interrupted".into(), ctx);
                    }
                    RemoteServerManagerEvent::SessionConnectionFailed { session_id, error, is_cancelled, .. }
                        if *session_id == me.session_id && !is_cancelled => {
                        me.failed(error.clone(), ctx);
                    }
                    _ => {}
                }
            });
            let mut owner = Self { recipe, directory, slot: ConnectionSlot::default(), master: None,
                session_id: SessionId::from(0), lifetime: None, status: ConnectionStatus::Connecting,
                retry_at: None, attempts: 0 };
            owner.connect(ctx);
            owner.schedule_tick(ctx);
            owner
        }))
    }

    pub fn recipe(&self) -> &SshRecipe { &self.recipe }
    pub fn slot(&self) -> ConnectionSlot { self.slot.clone() }
    pub fn socket_path(&self) -> PathBuf {
        // OpenSSH unlinks its socket during exit. An older master must never
        // remove the socket of an already-started replacement attempt.
        self.directory.path().join(format!("{:016x}", self.session_id.as_u64()))
    }
    pub fn status(&self) -> &ConnectionStatus { &self.status }

    fn failed(&mut self, error: String, ctx: &mut ModelContext<Self>) {
        if self.retry_at.is_some() { return; }
        self.slot.replace(None);
        self.attempts = self.attempts.saturating_add(1);
        self.status = ConnectionStatus::Reconnecting { attempt: self.attempts, error };
        self.retry_at = Some(Instant::now() + retry_delay(self.attempts));
        ctx.emit(());
    }

    fn schedule_tick(&mut self, ctx: &mut ModelContext<Self>) {
        ctx.spawn(async { async_io::Timer::after(Duration::from_secs(1)).await; }, |me, _, ctx| {
            if me.retry_at.is_some_and(|deadline| Instant::now() >= deadline) {
                me.connect(ctx);
            } else if matches!(me.status, ConnectionStatus::Connected) {
                let dead = me.master.as_mut().is_none_or(|master| !matches!(master.try_status(), Ok(None)));
                if dead || me.slot.snapshot().1.is_none() {
                    me.failed("SSH connection was interrupted".into(), ctx);
                }
            }
            me.schedule_tick(ctx);
        });
    }

    fn connect(&mut self, ctx: &mut ModelContext<Self>) {
        self.retry_at = None;
        self.slot.replace(None);
        // The cleanup receiver deregisters the OLD ID; late completion cannot
        // attach to a new attempt, since every attempt has a fresh ID.
        self.lifetime.take();
        self.master.take();
        self.session_id = SessionId::from(rand::random::<u64>());
        let session_id = self.session_id;
        let (lifetime, ended) = async_channel::bounded::<()>(1);
        self.lifetime = Some(lifetime);
        RemoteServerManager::handle(ctx).update(ctx, |_, ctx| {
            ctx.spawn(async move { let _ = ended.recv().await; }, move |manager, _, ctx| {
                manager.deregister_session(session_id, ctx);
            });
        });
        let recipe = self.recipe.clone();
        let directory = self.directory.clone();
        let socket = self.socket_path();
        let executor = ctx.background_executor().clone();
        ctx.spawn(async move {
            let mut child = Command::new_with_session("ssh")
                .args(recipe.master_arguments(&socket)?)
                .current_dir(recipe.working_directory())
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped())
                .kill_on_drop(true).spawn()?;
            let tail = Arc::new(Mutex::new(Vec::<u8>::new()));
            if let Some(mut stderr) = child.stderr.take() {
                let tail = tail.clone();
                executor.spawn(async move {
                    let mut bytes = [0_u8; 1024];
                    while let Ok(count) = stderr.read(&mut bytes).await {
                        if count == 0 { break; }
                        let mut tail = tail.lock();
                        tail.extend_from_slice(&bytes[..count]);
                        let excess = tail.len().saturating_sub(8192);
                        tail.drain(..excess);
                    }
                }).detach();
            }
            let result = wait_for_master(&mut child, &socket, &tail).await;
            // Hold the directory until spawn/readiness has finished even if
            // the owning view disappeared while this task was running.
            drop(directory);
            result?;
            Ok(child)
        }, move |me, result: Result<Child>, ctx| {
            if me.session_id != session_id { return; }
            match result {
                Ok(child) => {
                    me.master = Some(child);
                    let auth_state = AuthStateProvider::as_ref(ctx).get().clone();
                    let auth_client = ServerApiProvider::as_ref(ctx).get_auth_client();
                    let crash_reporting = PrivacySettings::handle(ctx).as_ref(ctx).is_crash_reporting_enabled;
                    let auth = Arc::new(server_api_auth_context(auth_state, auth_client, crash_reporting));
                    // This owner, not RemoteServerManager, owns this master.
                    let transport = SshTransport::new(me.socket_path(), auth.clone(), false);
                    RemoteServerManager::handle(ctx).update(ctx, |manager, ctx| {
                        manager.connect_session(session_id, transport, auth,
                            Some("Persistent SSH workspace".into()), ctx);
                    });
                }
                Err(error) => me.failed(format!("{error:#}"), ctx),
            }
        });
    }
}

async fn wait_for_master(child: &mut Child, socket: &Path, tail: &Mutex<Vec<u8>>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_status()? {
            let error = String::from_utf8_lossy(&tail.lock()).trim().to_owned();
            bail!("SSH authentication/connection failed ({status}): {error}");
        }
        if socket.exists() {
            let result = Command::new("ssh").args(["-S"]).arg(socket)
                .args(["-O", "check", "placeholder"])
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
                .kill_on_drop(true).status().with_timeout(Duration::from_secs(2)).await;
            if matches!(result, Ok(Ok(status)) if status.success()) { return Ok(()); }
        }
        if Instant::now() >= deadline { return Err(anyhow!("Timed out establishing a private SSH connection")); }
        async_io::Timer::after(Duration::from_millis(150)).await;
    }
}

impl Drop for ConnectionOwner {
    fn drop(&mut self) {
        self.slot.replace(None);
        // Child::kill_on_drop closes this owner's SSH master, not the tmux job.
        self.master.take();
        self.lifetime.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_ssh_master_socket_directory_is_private() {
        let directory = private_master_directory().unwrap();
        assert_eq!(directory.path().metadata().unwrap().permissions().mode() & 0o777, 0o700);
    }
    #[test]
    fn persistent_reconnect_backoff_is_bounded_and_does_not_overflow() {
        let delays: Vec<_> = (1..=8).map(|attempt| retry_delay(attempt).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(retry_delay(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn persistent_reconnect_attempts_do_not_reuse_a_socket_path() {
        let recipe = serde_json::from_value(serde_json::json!({
            "working_directory": "/tmp", "arguments": ["lab"]
        })).unwrap();
        let mut owner = ConnectionOwner { recipe,
            directory: Arc::new(tempfile::tempdir().unwrap()), slot: ConnectionSlot::default(), master: None,
            session_id: SessionId::from(1), lifetime: None, status: ConnectionStatus::Connecting,
            retry_at: None, attempts: 0 };
        let old_socket = owner.socket_path();
        owner.session_id = SessionId::from(2);
        assert_ne!(old_socket, owner.socket_path(),
            "Old SSH process cleanup must not unlink the replacement master's socket");
    }
}
