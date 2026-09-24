use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, mpsc::SyncSender};

use anyhow::{Result, anyhow};
use parking_lot::FairMutex;
use pathfinder_geometry::vector::Vector2F;
use warp_core::SessionId;
use warpui::{AppContext, ModelHandle, ViewHandle, WindowId};

use super::bootstrap::profile_for_shell;
use super::connection::{ConnectionSlot, WorkspaceCommandExecutor};
use super::replay::NativeReplay;
use super::transport::{Transport, TransportHandle};
use crate::ai::blocklist::InputConfig;
use crate::context_chips::prompt_type::PromptType;
use crate::pane_group::TerminalViewResources;
use crate::persistence::ModelEvent;
use crate::terminal::event_listener::ChannelEventListener;
use crate::terminal::model::session::Sessions;
use crate::terminal::model_events::{ModelEventDispatcher, SshRemoteServerSupport};
use crate::terminal::shell::{ShellName, ShellType};
use crate::terminal::terminal_manager::BlockSpacing;
use crate::terminal::writeable_pty::{
    PtyController,
    terminal_manager_util::{init_pty_controller_model, wire_up_pty_controller_with_surface},
};
use crate::terminal::{ShellLaunchState, TerminalModel, TerminalView, terminal_manager};

#[derive(Clone, Debug)]
pub struct WorkspaceAttachment {
    pub workspace_id: String,
    pub generation: String,
    pub shell_type: ShellType,
    pub control_path: PathBuf,
    pub connection: ConnectionSlot,
    pub owner: ModelHandle<super::connection_owner::ConnectionOwner>,
}

pub struct TerminalManager {
    model: Arc<FairMutex<TerminalModel>>,
    pub transport: TransportHandle,
    _controller: ModelHandle<PtyController<TransportHandle>>,
    _view: ViewHandle<TerminalView>,
}

pub struct TerminalManagerInit {
    pub manager: ModelHandle<Box<dyn crate::terminal::TerminalManager>>,
    pub view: ViewHandle<TerminalView>,
}

impl TerminalManager {
    #[allow(clippy::too_many_arguments)]
    pub fn create_model(
        attachment: WorkspaceAttachment,
        resources: TerminalViewResources,
        initial_size: Vector2F,
        model_event_sender: Option<SyncSender<ModelEvent>>,
        window_id: WindowId,
        initial_input_config: Option<InputConfig>,
        ctx: &mut AppContext,
    ) -> Result<TerminalManagerInit> {
        let id = remote_server::persistent_shell::session_id(&attachment.generation)
            .ok_or_else(|| anyhow!("Invalid workspace incarnation"))?;
        let session_id = SessionId::from(id);
        let shell = profile_for_shell(attachment.shell_type)?.shell;
        let (wakeups_tx, wakeups_rx) = async_channel::unbounded();
        let (events_tx, events_rx) = async_channel::unbounded();
        let (executor_tx, executor_rx) = async_channel::unbounded();
        let (pty_reads_tx, _pty_reads_rx) = async_broadcast::broadcast(1);
        let events = ChannelEventListener::new(wakeups_tx, events_tx, pty_reads_tx);
        let executor = Arc::new(WorkspaceCommandExecutor {
            session_id,
            connection: attachment.connection.clone(),
        });
        let sessions =
            ctx.add_model(|ctx| Sessions::new(executor_tx, ctx).with_command_executor(executor));
        let model_events = ctx.add_model(|ctx| {
            ModelEventDispatcher::new_with_ssh_remote_server_support(
                events_rx,
                sessions.clone(),
                SshRemoteServerSupport::Disabled,
                ctx,
            )
        });
        let mut model = terminal_manager::create_terminal_model(
            None,
            None,
            initial_size,
            events.clone(),
            ShellLaunchState::ShellSpawned {
                available_shell: None,
                display_name: ShellName::blank(),
                shell_type: attachment.shell_type,
            },
            BlockSpacing::for_gui(ctx),
            ctx,
        );
        model.configure_persistent_ssh_session(
            session_id,
            attachment.control_path.clone(),
            shell.clone(),
        );
        let size_info = *model.block_list().size();
        let colors = model.colors();
        let model = Arc::new(FairMutex::new(model));
        let (handle, receiver) = TransportHandle::channel();
        let input_gate = handle.clone();
        model
            .lock()
            .set_command_input_gate(Arc::new(move || input_gate.accepts_input()));
        let controller = init_pty_controller_model(
            handle.clone(),
            executor_rx,
            model_events.clone(),
            sessions.clone(),
            model.clone(),
            ctx,
        );
        let prompt_type =
            ctx.add_model(|ctx| PromptType::new_dynamic_from_sessions(sessions.clone(), ctx));
        let view = ctx.add_typed_action_view(window_id, |ctx| {
            TerminalView::new(
                resources,
                wakeups_rx,
                model_events.clone(),
                model.clone(),
                sessions.clone(),
                size_info,
                colors,
                model_event_sender.clone(),
                prompt_type,
                initial_input_config,
                None,
                None,
                false,
                ctx,
            )
        });
        wire_up_pty_controller_with_surface(
            &controller,
            &view,
            model.clone(),
            sessions,
            model_event_sender,
            ctx,
        );
        let status = ctx.add_typed_action_view(window_id, |ctx| {
            super::status_view::StatusView::new(
                handle.clone(),
                attachment.owner.clone(),
                model.clone(),
                &attachment.workspace_id,
                view.id(),
                ctx,
            )
        });
        view.update(ctx, |view, _| {
            view.persistent_workspace = Some(attachment.clone());
            view.persistent_transport = Some(handle.clone());
            view.persistent_status = Some(status);
        });
        let worker = Transport {
            workspace_id: attachment.workspace_id,
            generation: attachment.generation,
            session_id,
            shell,
            connection: attachment.connection,
            replay: NativeReplay::new(model.clone(), events),
            handle: handle.clone(),
            receiver,
            size: size_info,
        };
        ctx.background_executor().spawn(worker.run()).detach();
        let terminal_view = view.clone();
        let manager = ctx.add_model(|_| {
            Box::new(Self {
                model,
                transport: handle,
                _controller: controller,
                _view: view,
            }) as Box<dyn crate::terminal::TerminalManager>
        });
        Ok(TerminalManagerInit {
            manager,
            view: terminal_view,
        })
    }
}

impl Drop for TerminalManager {
    fn drop(&mut self) {
        self.transport.detach();
    }
}

impl crate::terminal::TerminalManager for TerminalManager {
    fn model(&self) -> Arc<FairMutex<TerminalModel>> {
        self.model.clone()
    }
    fn on_view_detached(
        &self,
        detach_type: crate::pane_group::pane::DetachType,
        _ctx: &mut AppContext,
    ) {
        use crate::pane_group::pane::DetachType;
        match detach_type {
            DetachType::Closed => self.transport.detach(),
            DetachType::HiddenForClose => self.transport.pause(),
            DetachType::Moved => {}
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
