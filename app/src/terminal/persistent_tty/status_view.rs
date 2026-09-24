//! One workspace chip, hosted by the prompt or its hidden-input fallback.
use std::sync::Arc;
use std::time::Duration;

use parking_lot::FairMutex;
use pathfinder_geometry::vector::vec2f;
use warpui::elements::{
    ChildAnchor, ChildView, Container, Flex, Hoverable, MouseStateHandle, OffsetPositioning,
    ParentAnchor, ParentElement, ParentOffsetBounds, Stack, Text,
};
use warpui::platform::Cursor;
use warpui::{
    AppContext, Element, Entity, EntityId, ModelHandle, TypedActionView, View, ViewContext,
    ViewHandle,
};

use super::connection_owner::{ConnectionOwner, ConnectionStatus};
use super::transport::{TransportHandle, TransportPhase, TransportStatus};
use crate::appearance::Appearance;
use crate::context_chips::display_chip::{UdiChipConfig, render_udi_chip};
use crate::context_chips::persistent_workspace_popup::{
    PersistentWorkspacePopup, PickerConnection, WorkspaceKey,
};
use crate::terminal::TerminalModel;
use crate::ui_components::blended_colors;
use crate::ui_components::icons::Icon;
use warpui::SingletonEntity;

#[derive(Clone, Debug)]
pub enum Action {
    AcknowledgeInput,
    OpenWorkspaces,
    RetryOutput,
    OpenRecovery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StatusText {
    label: String,
    message: String,
    show_details: bool,
    acknowledge: bool,
    retry: bool,
    recover: bool,
}

fn status_text(
    transport: &TransportStatus,
    connection: &ConnectionStatus,
    running: bool,
) -> StatusText {
    let label = match &transport.phase {
        TransportPhase::Live if transport.input_delivery_unknown || transport.discarded_input => {
            "Input paused"
        }
        TransportPhase::Live if running => "Running",
        TransportPhase::Live => "Idle",
        TransportPhase::Replaying => "Restoring",
        TransportPhase::Exited => "Exited",
        TransportPhase::Removed => "Removed",
        TransportPhase::HistoryGap { .. } => "History expired",
        TransportPhase::Failed(_) => "Paused",
        TransportPhase::Disconnected => match connection {
            ConnectionStatus::Connecting => "Connecting",
            ConnectionStatus::Connected => "Attaching",
            ConnectionStatus::Reconnecting { .. } => "Reconnecting",
        },
    }
    .to_owned();
    let message = match &transport.phase {
        TransportPhase::Live => if running {
            "Running | persistent SSH"
        } else {
            "Idle | persistent SSH"
        }
        .into(),
        TransportPhase::Replaying => "Restoring remote output | input paused".into(),
        TransportPhase::Exited => "Shell exited | retained output".into(),
        TransportPhase::Removed => {
            "Workspace was removed or replaced | no new job was created".into()
        }
        TransportPhase::HistoryGap { earliest_cursor } => format!(
            "Remote history before byte {earliest_cursor} expired. Native block restoration is paused; the job was not terminated."
        ),
        TransportPhase::Failed(error) => format!("Workspace paused: {error}"),
        TransportPhase::Disconnected => match connection {
            ConnectionStatus::Connecting => {
                "Connecting | remote job is independent of this tab".into()
            }
            ConnectionStatus::Connected => "Attaching to remote workspace".into(),
            ConnectionStatus::Reconnecting { attempt, error } => {
                format!("Reconnecting (attempt {attempt}) | input paused | {error}")
            }
        },
    };
    let message = if let Some(bytes) = transport.history_storage_bytes
        && bytes >= 1024 * 1024 * 1024
    {
        format!(
            "{message}\nRetained history uses {:.1} GiB on the remote host. Delete unused workspaces to free space.",
            bytes as f64 / (1024.0 * 1024.0 * 1024.0)
        )
    } else {
        message
    };
    let acknowledge = transport.input_delivery_unknown || transport.discarded_input;
    let message = if transport.input_delivery_unknown {
        format!(
            "{message}\nSome input may have reached the server. Inspect the output before resuming; nothing was resent."
        )
    } else if transport.discarded_input {
        format!("{message}\nInput queued during the connection change was discarded, not resent.")
    } else {
        message
    };
    StatusText {
        label,
        message,
        show_details: !matches!(transport.phase, TransportPhase::Live)
            || acknowledge
            || transport
                .history_storage_bytes
                .is_some_and(|bytes| bytes >= 1024 * 1024 * 1024),
        acknowledge,
        retry: matches!(transport.phase, TransportPhase::Failed(_)),
        recover: matches!(
            transport.phase,
            TransportPhase::Failed(_) | TransportPhase::HistoryGap { .. }
        ),
    }
}

pub struct StatusView {
    workspace_label: String,
    transport: TransportHandle,
    owner: ModelHandle<ConnectionOwner>,
    model: Arc<FairMutex<TerminalModel>>,
    status: StatusText,
    acknowledge_mouse: MouseStateHandle,
    picker_mouse: MouseStateHandle,
    retry_mouse: MouseStateHandle,
    recovery_mouse: MouseStateHandle,
    picker: ViewHandle<PersistentWorkspacePopup>,
    picker_open: bool,
    terminal_view_id: EntityId,
}

impl StatusView {
    pub fn new(
        transport: TransportHandle,
        owner: ModelHandle<ConnectionOwner>,
        model: Arc<FairMutex<TerminalModel>>,
        workspace_id: &str,
        terminal_view_id: EntityId,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let status = status_text(&transport.status(), owner.as_ref(ctx).status(), false);
        ctx.subscribe_to_model(&owner, |me, _, _, ctx| {
            me.refresh(ctx);
        });
        let picker = ctx.add_typed_action_view(|_| PersistentWorkspacePopup::new(terminal_view_id));
        ctx.subscribe_to_view(&picker, |me, _, _, ctx| {
            me.picker_open = false;
            if let Some(view) = ctx
                .view_with_id::<crate::terminal::TerminalView>(ctx.window_id(), me.terminal_view_id)
            {
                ctx.focus(&view);
            }
            ctx.notify();
        });
        let mut view = Self {
            workspace_label: workspace_id.chars().take(8).collect(),
            transport,
            owner,
            model,
            status,
            acknowledge_mouse: Default::default(),
            picker_mouse: Default::default(),
            retry_mouse: Default::default(),
            recovery_mouse: Default::default(),
            picker,
            picker_open: false,
            terminal_view_id,
        };
        view.schedule_tick(ctx);
        view
    }

    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        let running = self.model.lock().block_list().active_block().is_executing();
        let status = status_text(
            &self.transport.status(),
            self.owner.as_ref(ctx).status(),
            running,
        );
        if status != self.status {
            self.status = status;
            ctx.notify();
        }
    }

    fn schedule_tick(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.spawn(
            async {
                async_io::Timer::after(Duration::from_millis(250)).await;
            },
            |me, _, ctx| {
                me.refresh(ctx);
                me.schedule_tick(ctx);
            },
        );
    }
}

impl Entity for StatusView {
    type Event = ();
}

impl TypedActionView for StatusView {
    type Action = Action;
    fn handle_action(&mut self, action: &Action, ctx: &mut ViewContext<Self>) {
        match action {
            Action::OpenRecovery => {
                if !self.status.recover {
                    return;
                }
                if let Some(view) = ctx.view_with_id::<crate::terminal::TerminalView>(
                    ctx.window_id(),
                    self.terminal_view_id,
                ) && let Some(attachment) = view.as_ref(ctx).persistent_workspace.clone()
                {
                    view.update(ctx, |_, ctx| {
                        ctx.emit(crate::terminal::view::Event::OpenPersistentRecovery(
                            attachment,
                        ))
                    });
                }
            }
            Action::RetryOutput => self.transport.retry_output(),
            Action::AcknowledgeInput => {
                self.transport.acknowledge_uncertain_input();
                self.refresh(ctx);
            }
            Action::OpenWorkspaces => {
                let attachment = ctx
                    .view_with_id::<crate::terminal::TerminalView>(
                        ctx.window_id(),
                        self.terminal_view_id,
                    )
                    .and_then(|view| view.as_ref(ctx).persistent_workspace.clone());
                let connection = attachment.map(|attachment| PickerConnection {
                    connection: attachment.connection,
                    control_path: attachment.owner.as_ref(ctx).socket_path(),
                    shell: attachment.shell_type,
                    current_workspace: Some(WorkspaceKey {
                        id: attachment.workspace_id,
                        generation: attachment.generation,
                    }),
                    recipe: Some(attachment.owner.as_ref(ctx).recipe().clone()),
                    owner: Some(attachment.owner),
                });
                self.picker_open = true;
                self.picker
                    .update(ctx, |picker, ctx| picker.open(connection, ctx));
                ctx.notify();
            }
        }
    }
}

impl View for StatusView {
    fn ui_name() -> &'static str {
        "PersistentWorkspaceStatus"
    }
    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let background = theme.surface_2();
        let foreground = blended_colors::text_main(theme, background);
        let font = appearance.ui_font_family();
        let size = appearance.ui_font_size();
        let switcher = Hoverable::new(self.picker_mouse.clone(), move |state| {
            render_udi_chip(
                UdiChipConfig::new_with_icon(
                    Icon::Terminal,
                    theme.ansi_fg_blue(),
                    format!("tmux {} | {}", self.workspace_label, self.status.label),
                )
                .with_hovered(state.is_hovered()),
                appearance,
            )
        })
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(Action::OpenWorkspaces))
        .with_cursor(Cursor::PointingHand)
        .finish();
        let mut column = Flex::column().with_child(switcher);
        if self.status.show_details {
            column.add_child(
                Text::new(self.status.message.clone(), font, size)
                    .with_color(foreground)
                    .soft_wrap(true)
                    .finish(),
            );
        }
        if self.status.acknowledge {
            column.add_child(
                Hoverable::new(self.acknowledge_mouse.clone(), move |_| {
                    Container::new(
                        Text::new("Resume input without resending", font, size)
                            .with_color(theme.ansi_fg_yellow())
                            .finish(),
                    )
                    .with_vertical_padding(5.)
                    .finish()
                })
                .on_click(|ctx, _, _| ctx.dispatch_typed_action(Action::AcknowledgeInput))
                .with_cursor(Cursor::PointingHand)
                .finish(),
            );
        }
        if self.status.retry {
            column.add_child(
                Hoverable::new(self.retry_mouse.clone(), move |_| {
                    Container::new(
                        Text::new("Retry output (never resends input)", font, size)
                            .with_color(theme.ansi_fg_blue())
                            .finish(),
                    )
                    .with_vertical_padding(5.)
                    .finish()
                })
                .on_click(|ctx, _, _| ctx.dispatch_typed_action(Action::RetryOutput))
                .with_cursor(Cursor::PointingHand)
                .finish(),
            );
        }
        if self.status.recover {
            column.add_child(Text::new("Missing output cannot be reconstructed. Recovery opens the live tmux screen in a separate pane; it does not restart the job or restore missing blocks.", font, size)
                .with_color(foreground).soft_wrap(true).finish());
            column.add_child(
                Hoverable::new(self.recovery_mouse.clone(), move |_| {
                    Container::new(
                        Text::new("Open live recovery terminal", font, size)
                            .with_color(theme.ansi_fg_blue())
                            .finish(),
                    )
                    .with_vertical_padding(5.)
                    .finish()
                })
                .on_click(|ctx, _, _| ctx.dispatch_typed_action(Action::OpenRecovery))
                .with_cursor(Cursor::PointingHand)
                .finish(),
            );
        }
        let mut stack = Stack::new().with_child(column.finish());
        if self.picker_open {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.picker).finish(),
                OffsetPositioning::offset_from_parent(
                    vec2f(0., -4.),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::TopLeft,
                    ChildAnchor::BottomLeft,
                ),
            );
        }
        stack.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn healthy_workspace_has_only_a_compact_chip() {
        let (handle, _) = TransportHandle::channel();
        let mut status = handle.status();
        status.phase = TransportPhase::Live;
        for (running, label) in [(false, "Idle"), (true, "Running")] {
            let text = status_text(&status, &ConnectionStatus::Connected, running);
            assert_eq!(text.label, label);
            assert!(!text.show_details);
        }
    }

    #[test]
    fn compact_chip_does_not_hide_input_or_storage_warnings() {
        let (handle, _) = TransportHandle::channel();
        let mut status = handle.status();
        status.phase = TransportPhase::Live;
        status.discarded_input = true;
        let text = status_text(&status, &ConnectionStatus::Connected, false);
        assert_eq!(text.label, "Input paused");
        assert!(text.show_details && text.acknowledge);
        status.discarded_input = false;
        status.history_storage_bytes = Some(1024 * 1024 * 1024);
        let text = status_text(&status, &ConnectionStatus::Connected, false);
        assert!(text.show_details);
        assert!(text.message.contains("GiB"));
    }
    #[test]
    fn persistent_status_never_reports_a_disconnected_job_as_idle() {
        let (handle, _) = TransportHandle::channel();
        let text = status_text(
            &handle.status(),
            &ConnectionStatus::Reconnecting {
                attempt: 2,
                error: "Network unavailable".into(),
            },
            false,
        );
        assert!(text.message.contains("Reconnecting"));
        assert!(!text.message.contains("Idle"));
    }
    #[test]
    fn persistent_status_exposes_delivery_uncertainty_until_acknowledged() {
        let (handle, _) = TransportHandle::channel();
        let mut status = handle.status();
        status.phase = TransportPhase::Live;
        status.input_delivery_unknown = true;
        let text = status_text(&status, &ConnectionStatus::Connected, true);
        assert!(text.message.contains("Running"));
        assert!(text.message.contains("nothing was resent"));
        assert!(text.acknowledge);
    }

    #[test]
    fn persistent_expired_history_does_not_offer_a_misleading_read_retry() {
        let (handle, _) = TransportHandle::channel();
        let mut status = handle.status();
        status.phase = TransportPhase::HistoryGap {
            earliest_cursor: 1048576,
        };
        let text = status_text(&status, &ConnectionStatus::Connected, true);
        assert!(text.message.contains("1048576"));
        assert!(text.message.contains("job was not terminated"));
        assert!(!text.retry);
        assert!(!text.acknowledge);
        assert!(text.recover);
    }
}
