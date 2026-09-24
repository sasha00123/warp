//! A replaceable extension connection shared by terminal IO and completions.
//! Replacing the client never changes the remote workspace incarnation.
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use parking_lot::RwLock;
use remote_server::client::RemoteServerClient;
use warp_completer::completer::CommandOutput;
use warp_core::SessionId;

use crate::terminal::model::session::command_executor::remote_server_executor::RemoteServerCommandExecutor;
use crate::terminal::model::session::command_executor::{CommandExecutor, ExecuteCommandOptions};
use crate::terminal::shell::Shell;

#[derive(Clone, Default)]
pub struct ConnectionSlot(Arc<RwLock<ConnectionState>>);

#[derive(Default)]
struct ConnectionState {
    epoch: u64,
    client: Option<Arc<RemoteServerClient>>,
}

impl std::fmt::Debug for ConnectionSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionSlot")
            .field("epoch", &self.0.read().epoch)
            .finish_non_exhaustive()
    }
}

impl ConnectionSlot {
    /// The connection owner calls this only after extension authentication.
    pub fn replace(&self, client: Option<Arc<RemoteServerClient>>) {
        let mut state = self.0.write();
        state.epoch = state
            .epoch
            .checked_add(1)
            .expect("connection epoch exhausted");
        state.client = client;
    }

    pub(crate) fn snapshot(&self) -> (u64, Option<Arc<RemoteServerClient>>) {
        let state = self.0.read();
        (
            state.epoch,
            state
                .client
                .clone()
                .filter(|client| !client.is_disconnected()),
        )
    }
}

#[derive(Debug)]
pub(super) struct WorkspaceCommandExecutor {
    pub session_id: SessionId,
    pub connection: ConnectionSlot,
}

#[async_trait]
impl CommandExecutor for WorkspaceCommandExecutor {
    async fn execute_command(
        &self,
        command: &str,
        shell: &Shell,
        directory: Option<&str>,
        environment: Option<HashMap<String, String>>,
        options: ExecuteCommandOptions,
    ) -> Result<CommandOutput> {
        let (_, client) = self.connection.snapshot();
        let client = client.ok_or_else(|| anyhow!("Persistent workspace is disconnected"))?;
        RemoteServerCommandExecutor::new(self.session_id, client)
            .execute_command(command, shell, directory, environment, options)
            .await
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn supports_parallel_command_execution(&self) -> bool {
        true
    }
}
