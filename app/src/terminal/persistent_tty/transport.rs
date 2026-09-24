//! Single ordered writer/reader for a persistent terminal. Never retries input.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use async_channel::{Receiver, Sender};
use futures_util::{FutureExt, select_biased};
use parking_lot::Mutex;
use remote_server::client::RemoteServerClient;
use remote_server::persistent_replay::{ReplayError, ReplayPhase};
use remote_server::proto::{
    PersistentTerminalRequest, PersistentWorkspaceOperation, PersistentWorkspaceRequest,
    PersistentWorkspaceResponse, persistent_terminal_request::Operation,
};
use warp_core::SessionId;

use super::connection::ConnectionSlot;
use super::replay::NativeReplay;
use crate::terminal::SizeInfo;
use crate::terminal::writeable_pty::Message;
use crate::terminal::writeable_pty::pty_controller::{EventLoopSendError, EventLoopSender};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportPhase {
    Disconnected,
    Replaying,
    Live,
    Exited,
    Removed,
    HistoryGap { earliest_cursor: u64 },
    Failed(String),
}

fn phase_after_replay(phase: ReplayPhase, shell_ready: bool, exited: bool) -> TransportPhase {
    match phase {
        ReplayPhase::Live if exited => TransportPhase::Replaying,
        ReplayPhase::Live if shell_ready => TransportPhase::Live,
        ReplayPhase::Live | ReplayPhase::Replaying => TransportPhase::Replaying,
        ReplayPhase::Closed if exited => TransportPhase::Exited,
        ReplayPhase::Closed | ReplayPhase::Disconnected => TransportPhase::Failed(
            "Output recorder stopped; input is paused. The remote job was not terminated.".into(),
        ),
        ReplayPhase::HistoryGap { earliest_cursor } => {
            TransportPhase::HistoryGap { earliest_cursor }
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
struct WorkspaceRequestError {
    code: String,
    message: String,
}

fn transient_request_failure(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<WorkspaceRequestError>()
        .is_some_and(|error| error.code == "tmux_timeout")
}

#[derive(Clone, Debug)]
pub struct TransportStatus {
    pub phase: TransportPhase,
    pub input_delivery_unknown: bool,
    pub discarded_input: bool,
    pub history_storage_bytes: Option<u64>,
    epoch: u64,
}

#[derive(Clone)]
pub struct TransportHandle {
    tx: Sender<QueuedMessage>,
    status: Arc<Mutex<TransportStatus>>,
    stopped: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    lifecycle: Arc<AtomicU64>,
    retry_output: Arc<AtomicBool>,
}

pub(super) struct QueuedMessage {
    epoch: u64,
    lifecycle: u64,
    message: Message,
}

impl TransportHandle {
    pub(super) fn channel() -> (Self, Receiver<QueuedMessage>) {
        let (tx, rx) = async_channel::bounded(128);
        (
            Self {
                tx,
                status: Arc::new(Mutex::new(TransportStatus {
                    phase: TransportPhase::Disconnected,
                    input_delivery_unknown: false,
                    discarded_input: false,
                    history_storage_bytes: None,
                    epoch: 0,
                })),
                stopped: Arc::new(AtomicBool::new(false)),
                paused: Arc::new(AtomicBool::new(false)),
                lifecycle: Arc::new(AtomicU64::new(0)),
                retry_output: Arc::new(AtomicBool::new(false)),
            },
            rx,
        )
    }

    pub fn status(&self) -> TransportStatus {
        self.status.lock().clone()
    }

    /// Only restarts cursor-based reads. Does not acknowledge or resend input.
    pub fn retry_output(&self) {
        self.retry_output.store(true, Ordering::Release);
    }

    pub fn accepts_input(&self) -> bool {
        let status = self.status.lock();
        !self.stopped.load(Ordering::Acquire)
            && !self.paused.load(Ordering::Acquire)
            && status.phase == TransportPhase::Live
            && !status.input_delivery_unknown
            && !status.discarded_input
    }

    /// Explicit user acknowledgement, never an automatic reconnect side effect.
    pub fn acknowledge_uncertain_input(&self) {
        let mut status = self.status.lock();
        status.input_delivery_unknown = false;
        status.discarded_input = false;
    }

    /// Undo-close retains the model and its cursor. Pause reads without closing
    /// the queue; pending input from this attachment must never run on reopen.
    pub fn pause(&self) {
        let mut status = self.status.lock();
        if !self.paused.swap(true, Ordering::AcqRel) {
            self.lifecycle.fetch_add(1, Ordering::AcqRel);
        }
        status.phase = TransportPhase::Disconnected;
    }

    pub fn resume(&self) {
        let mut status = self.status.lock();
        if self.paused.swap(false, Ordering::AcqRel) {
            status.phase = TransportPhase::Disconnected;
        }
    }

    /// Detach this local reader, not the remote process or workspace.
    pub fn detach(&self) {
        self.stopped.store(true, Ordering::Release);
        self.status.lock().phase = TransportPhase::Disconnected;
        self.tx.close();
    }

    fn update(&self, epoch: u64, phase: TransportPhase) {
        let mut status = self.status.lock();
        status.epoch = epoch;
        status.phase = phase;
    }
}

impl EventLoopSender for TransportHandle {
    fn send(&self, message: Message) -> Result<(), EventLoopSendError> {
        if matches!(message, Message::Shutdown) {
            self.pause();
            return Ok(());
        }
        if matches!(message, Message::ChildExited) {
            self.detach();
            return Ok(());
        }
        let status = self.status.lock();
        if self.stopped.load(Ordering::Acquire)
            || (matches!(message, Message::Input(_))
                && (self.paused.load(Ordering::Acquire)
                    || status.phase != TransportPhase::Live
                    || status.input_delivery_unknown
                    || status.discarded_input))
        {
            return Err(EventLoopSendError::Other(anyhow!(
                "Workspace input is paused while disconnected, replaying, or awaiting delivery acknowledgement"
            )));
        }
        self.tx
            .try_send(QueuedMessage {
                epoch: status.epoch,
                lifecycle: self.lifecycle.load(Ordering::Acquire),
                message,
            })
            .map_err(|_| EventLoopSendError::Other(anyhow!("Workspace input queue is unavailable")))
    }
}

pub(super) struct Transport {
    pub workspace_id: String,
    pub generation: String,
    pub session_id: SessionId,
    pub shell: String,
    pub connection: ConnectionSlot,
    pub replay: NativeReplay,
    pub handle: TransportHandle,
    pub receiver: Receiver<QueuedMessage>,
    pub size: SizeInfo,
}

impl Transport {
    async fn request(
        &self,
        client: &RemoteServerClient,
        operation: Operation,
        input: Vec<u8>,
        cursor: u64,
    ) -> Result<PersistentWorkspaceResponse> {
        let response = client
            .persistent_workspace(PersistentWorkspaceRequest {
                protocol_version: 1,
                operation: PersistentWorkspaceOperation::Terminal as i32,
                workspace_id: self.workspace_id.clone(),
                generation: self.generation.clone(),
                terminal: Some(PersistentTerminalRequest {
                    operation: operation as i32,
                    cursor,
                    input,
                    columns: if operation == Operation::Resize {
                        self.size.columns as u32
                    } else {
                        0
                    },
                    rows: if operation == Operation::Resize {
                        self.size.rows as u32
                    } else {
                        0
                    },
                }),
                ..Default::default()
            })
            .await?;
        if response.protocol_version != 1 {
            bail!("Unsupported persistent terminal protocol");
        }
        if !response.terminal_transport_supported || !response.block_replay_supported {
            bail!(
                "The SSH extension cannot restore native persistent blocks. Install the matching custom extension."
            );
        }
        if let Some(error) = &response.error {
            return Err(WorkspaceRequestError {
                code: error.code.clone(),
                message: error.message.clone(),
            }
            .into());
        }
        if response.workspaces.len() != 1
            || response.workspaces[0].workspace_id != self.workspace_id
            || response.workspaces[0].generation != self.generation
        {
            bail!("Remote response refers to a different workspace incarnation");
        }
        Ok(response)
    }

    fn still_current(&self, epoch: u64, lifecycle: u64) -> bool {
        let (current, client) = self.connection.snapshot();
        !self.handle.stopped.load(Ordering::Acquire)
            && !self.handle.paused.load(Ordering::Acquire)
            && lifecycle == self.handle.lifecycle.load(Ordering::Acquire)
            && epoch == current
            && client.is_some()
    }

    fn failed(&mut self, epoch: u64, error: anyhow::Error, input_uncertain: bool) {
        self.replay.disconnected(input_uncertain);
        self.handle.status.lock().input_delivery_unknown |= input_uncertain;
        let phase = if error
            .downcast_ref::<WorkspaceRequestError>()
            .is_some_and(|error| {
                matches!(
                    error.code.as_str(),
                    "workspace_not_found" | "stale_workspace"
                )
            }) {
            TransportPhase::Removed
        } else {
            TransportPhase::Failed(error.to_string())
        };
        self.handle.update(epoch, phase);
    }

    async fn request_failed(
        &mut self,
        epoch: u64,
        error: anyhow::Error,
        input_uncertain: bool,
    ) -> bool {
        let retry = transient_request_failure(&error);
        self.failed(epoch, error, input_uncertain);
        if retry {
            async_io::Timer::after(Duration::from_secs(1)).await;
        }
        retry
    }

    async fn write_input(
        &self,
        client: &RemoteServerClient,
        epoch: u64,
        lifecycle: u64,
        bytes: &[u8],
    ) -> Result<()> {
        // A lost acknowledgement may mean some or all bytes were delivered.
        // Never replay this batch, including its unsent suffix, on reconnect.
        for chunk in bytes.chunks(4096) {
            if !self.still_current(epoch, lifecycle) {
                bail!("Connection or tab attachment changed during input delivery");
            }
            self.request(client, Operation::Input, chunk.to_vec(), 0)
                .await?;
        }
        Ok(())
    }

    pub async fn run(mut self) {
        let mut active_epoch = None;
        let mut failed_epoch = None;
        let mut needs_attach = true;
        let mut needs_resize = true;
        let mut active_lifecycle = self.handle.lifecycle.load(Ordering::Acquire);
        while !self.handle.stopped.load(Ordering::Acquire) {
            let lifecycle = self.handle.lifecycle.load(Ordering::Acquire);
            if active_lifecycle != lifecycle {
                self.replay.disconnected(false);
                needs_attach = true;
                failed_epoch = None;
                active_lifecycle = lifecycle;
            }
            if self.handle.retry_output.swap(false, Ordering::AcqRel) {
                failed_epoch = None;
                needs_attach = true;
                self.replay.disconnected(false);
            }
            if !self.handle.status().input_delivery_unknown {
                self.replay.acknowledge_input_uncertainty();
            }
            let (epoch, client) = self.connection.snapshot();
            if active_epoch != Some(epoch) || client.is_none() {
                self.replay.disconnected(false);
                self.handle.update(epoch, TransportPhase::Disconnected);
                active_epoch = Some(epoch);
                needs_attach = true;
            }
            let Some(client) = client.filter(|_| {
                failed_epoch != Some(epoch) && !self.handle.paused.load(Ordering::Acquire)
            }) else {
                match self.next_message(Duration::from_millis(150)).await {
                    Some(QueuedMessage {
                        message: Message::Resize(size),
                        ..
                    }) => {
                        self.size = size;
                        needs_resize = true;
                    }
                    Some(QueuedMessage {
                        message: Message::Input(_),
                        ..
                    }) => {
                        self.handle.status.lock().discarded_input = true;
                    }
                    Some(_) | None => {}
                }
                continue;
            };
            if needs_attach {
                self.replay.connected();
                self.handle.update(epoch, TransportPhase::Replaying);
                client.notify_session_bootstrapped(self.session_id, &self.shell, None);
                needs_resize = true;
            }
            if let Ok(queued) = self.receiver.try_recv() {
                match queued.message {
                    Message::Resize(size) => {
                        self.size = size;
                        needs_resize = true;
                    }
                    Message::Input(bytes)
                        if queued.epoch == epoch
                            && queued.lifecycle == lifecycle
                            && self.handle.status().phase == TransportPhase::Live
                            && !self.handle.status().input_delivery_unknown
                            && !self.handle.status().discarded_input =>
                    {
                        if let Err(error) =
                            self.write_input(&client, epoch, lifecycle, &bytes).await
                        {
                            needs_attach = self.request_failed(epoch, error, true).await;
                            failed_epoch = (!needs_attach).then_some(epoch);
                            continue;
                        }
                    }
                    Message::Input(_) => {
                        self.handle.status.lock().discarded_input = true;
                    }
                    _ => {}
                }
            }
            let ticket = match self.replay.read_ticket() {
                Ok(ticket) => ticket,
                Err(_) => {
                    async_io::Timer::after(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let operation = if needs_attach {
                Operation::Attach
            } else {
                Operation::Read
            };
            let response = self
                .request(&client, operation, Vec::new(), ticket.cursor)
                .await;
            if !self.still_current(epoch, lifecycle) {
                continue;
            }
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    needs_attach = self.request_failed(epoch, error, false).await;
                    failed_epoch = (!needs_attach).then_some(epoch);
                    continue;
                }
            };
            needs_attach = false;
            let exited = response.workspaces[0].exited;
            self.handle.status.lock().history_storage_bytes =
                response.workspaces[0].history_storage_bytes;
            let Some(output) = response.terminal_output else {
                self.failed(epoch, anyhow!("Missing terminal output page"), false);
                failed_epoch = Some(epoch);
                continue;
            };
            let had_output = !output.output.is_empty();
            let replies = match self.replay.apply(ticket, output) {
                Ok(replies) => replies,
                Err(ReplayError::HistoryGap { earliest_cursor }) => {
                    self.handle
                        .update(epoch, TransportPhase::HistoryGap { earliest_cursor });
                    failed_epoch = Some(epoch);
                    continue;
                }
                Err(error) => {
                    self.failed(
                        epoch,
                        anyhow!("History cannot be replayed safely: {error:?}"),
                        false,
                    );
                    failed_epoch = Some(epoch);
                    continue;
                }
            };
            let phase = phase_after_replay(
                self.replay.phase(),
                self.replay.shell_ready(self.session_id),
                exited,
            );
            if phase == TransportPhase::Exited {
                self.replay.finish_shell(response.workspaces[0].exit_code);
            }
            if matches!(
                phase,
                TransportPhase::Failed(_) | TransportPhase::HistoryGap { .. }
            ) {
                failed_epoch = Some(epoch);
            }
            self.handle.update(epoch, phase);
            if !exited
                && !replies.is_empty()
                && let Err(error) = self.write_input(&client, epoch, lifecycle, &replies).await
            {
                needs_attach = self.request_failed(epoch, error, true).await;
                failed_epoch = (!needs_attach).then_some(epoch);
                continue;
            }
            if needs_resize && self.handle.status().phase == TransportPhase::Live {
                if let Err(error) = self
                    .request(&client, Operation::Resize, Vec::new(), 0)
                    .await
                {
                    needs_attach = self.request_failed(epoch, error, false).await;
                    failed_epoch = (!needs_attach).then_some(epoch);
                    continue;
                }
                needs_resize = false;
            }
            if !had_output {
                async_io::Timer::after(Duration::from_millis(35)).await;
            }
        }
        self.replay.disconnected(false);
    }

    async fn next_message(&self, delay: Duration) -> Option<QueuedMessage> {
        let message = self.receiver.recv().fuse();
        let timer = async_io::Timer::after(delay).fuse();
        futures_util::pin_mut!(message, timer);
        select_biased! { item = message => item.ok(), _ = timer => None }
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn persistent_timeout_recovery_does_not_retry_permanent_or_unknown_errors() {
        for code in [
            "tmux_timeout",
            "workspace_not_found",
            "stale_workspace",
            "invalid_request",
            "tmux_failed",
        ] {
            let error = WorkspaceRequestError {
                code: code.into(),
                message: "test".into(),
            }
            .into();
            assert_eq!(transient_request_failure(&error), code == "tmux_timeout");
        }
        assert!(!transient_request_failure(&anyhow!(
            "Unclassified protocol failure"
        )));
    }

    #[test]
    fn persistent_retry_output_never_acknowledges_or_requeues_input() {
        let (handle, receiver) = TransportHandle::channel();
        handle.status.lock().input_delivery_unknown = true;
        handle.status.lock().discarded_input = true;
        handle.retry_output();
        assert!(handle.retry_output.load(Ordering::Acquire));
        assert!(handle.status().input_delivery_unknown);
        assert!(handle.status().discarded_input);
        assert!(!handle.accepts_input());
        assert!(receiver.try_recv().is_err());
    }
}
