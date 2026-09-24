//! Native, out-of-band workspace selection. No command is typed into a pane.
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use remote_server::proto::{
    PersistentWorkspace, PersistentWorkspaceOperation, PersistentWorkspaceRequest,
    PersistentWorkspaceResponse,
};
use settings::Setting;
use warpui::elements::{
    ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container, CornerRadius,
    CrossAxisAlignment, Dismiss, DropShadow, Expanded, Flex, Hoverable, MainAxisSize,
    MouseStateHandle, ParentElement, Radius, ScrollbarWidth, Text,
};
use warpui::keymap::FixedBinding;
use warpui::platform::Cursor;
use warpui::{
    AppContext, Element, Entity, EntityId, SingletonEntity, TypedActionView, View, ViewContext,
};

use crate::appearance::Appearance;
use crate::terminal::TerminalView;
use crate::terminal::persistent_tty::{
    bootstrap::profile_for_shell, connection::ConnectionSlot, terminal_manager::WorkspaceAttachment,
};
use crate::terminal::shell::ShellType;
use crate::ui_components::blended_colors;

#[derive(Clone, Debug)]
pub struct PickerConnection {
    pub connection: ConnectionSlot,
    pub control_path: PathBuf,
    pub shell: ShellType,
    pub current_workspace: Option<WorkspaceKey>,
    pub owner: Option<
        warpui::ModelHandle<crate::terminal::persistent_tty::connection_owner::ConnectionOwner>,
    >,
    pub recipe: Option<crate::terminal::persistent_tty::ssh_recipe::SshRecipe>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceKey {
    pub id: String,
    pub generation: String,
}

impl From<&PersistentWorkspace> for WorkspaceKey {
    fn from(workspace: &PersistentWorkspace) -> Self {
        Self {
            id: workspace.workspace_id.clone(),
            generation: workspace.generation.clone(),
        }
    }
}

fn find_workspace<'a>(
    workspaces: &'a [PersistentWorkspace],
    key: &WorkspaceKey,
) -> Option<&'a PersistentWorkspace> {
    workspaces.iter().find(|workspace| {
        workspace.workspace_id == key.id && workspace.generation == key.generation
    })
}

fn workspace_status(workspace: &PersistentWorkspace) -> &'static str {
    if workspace.exited {
        return "exited";
    }
    match workspace.activity.as_deref() {
        Some("running") => "running",
        Some("idle") => "idle",
        Some("starting") => "starting",
        _ => "status unknown",
    }
}

fn history_label(bytes: Option<u64>) -> String {
    match bytes {
        Some(bytes) if bytes >= 1024 * 1024 * 1024 => format!(
            "History: {:.1} GiB on remote disk. Delete unused workspaces to free space.",
            bytes as f64 / (1024.0 * 1024.0 * 1024.0)
        ),
        Some(bytes) => format!(
            "History: {:.1} MiB on remote disk",
            bytes as f64 / (1024.0 * 1024.0)
        ),
        None => "History storage unavailable".into(),
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Close,
    Refresh,
    New,
    Select(WorkspaceKey),
    Terminate(WorkspaceKey),
    Up,
    Down,
    Enter,
}

pub enum PickerEvent {
    Close,
}

pub struct PersistentWorkspacePopup {
    terminal_view_id: EntityId,
    connection: Option<PickerConnection>,
    workspaces: Vec<PersistentWorkspace>,
    row_mouse: Vec<(MouseStateHandle, MouseStateHandle)>,
    new_mouse: MouseStateHandle,
    refresh_mouse: MouseStateHandle,
    scroll: ClippedScrollStateHandle,
    selected: usize,
    busy: bool,
    error: Option<String>,
    confirm_termination: Option<(String, String)>,
}

pub fn init(app: &mut AppContext) {
    use warpui::keymap::macros::*;
    app.register_fixed_bindings([
        FixedBinding::new(
            "escape",
            Action::Close,
            id!(PersistentWorkspacePopup::ui_name()),
        ),
        FixedBinding::new("up", Action::Up, id!(PersistentWorkspacePopup::ui_name())),
        FixedBinding::new(
            "down",
            Action::Down,
            id!(PersistentWorkspacePopup::ui_name()),
        ),
        FixedBinding::new(
            "enter",
            Action::Enter,
            id!(PersistentWorkspacePopup::ui_name()),
        ),
    ]);
}

impl PersistentWorkspacePopup {
    pub fn new(terminal_view_id: EntityId) -> Self {
        Self {
            terminal_view_id,
            connection: None,
            workspaces: Vec::new(),
            row_mouse: Vec::new(),
            new_mouse: Default::default(),
            refresh_mouse: Default::default(),
            scroll: Default::default(),
            selected: 0,
            busy: false,
            error: None,
            confirm_termination: None,
        }
    }

    pub fn open(&mut self, connection: Option<PickerConnection>, ctx: &mut ViewContext<Self>) {
        self.connection = connection;
        self.confirm_termination = None;
        self.refresh(ctx);
        ctx.focus_self();
    }

    fn client(&self) -> Result<std::sync::Arc<remote_server::client::RemoteServerClient>> {
        self.connection.as_ref().and_then(|connection| connection.connection.snapshot().1)
            .ok_or_else(|| anyhow!("The SSH extension is not connected. Connect it before opening a persistent workspace."))
    }

    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        if self.busy {
            return;
        }
        let client = match self.client() {
            Ok(client) => client,
            Err(error) => {
                self.error = Some(error.to_string());
                ctx.notify();
                return;
            }
        };
        self.busy = true;
        self.error = None;
        ctx.notify();
        ctx.spawn(
            async move {
                checked(
                    client
                        .persistent_workspace(PersistentWorkspaceRequest {
                            protocol_version: 1,
                            operation: PersistentWorkspaceOperation::List as i32,
                            ..Default::default()
                        })
                        .await?,
                )
            },
            |me, result: Result<PersistentWorkspaceResponse>, ctx| {
                me.busy = false;
                match result {
                    Ok(response) => {
                        me.workspaces = response.workspaces;
                        me.row_mouse = (0..me.workspaces.len())
                            .map(|_| Default::default())
                            .collect();
                        me.selected = me.selected.min(me.workspaces.len());
                    }
                    Err(error) => me.error = Some(error.to_string()),
                }
                ctx.notify();
            },
        );
    }

    fn create(&mut self, ctx: &mut ViewContext<Self>) {
        if self.busy {
            return;
        }
        let setup: Result<_> = (|| {
            let client = self.client()?;
            let connection = self
                .connection
                .as_ref()
                .ok_or_else(|| anyhow!("No SSH connection"))?;
            Ok((client, profile_for_shell(connection.shell)?))
        })();
        let (client, profile) = match setup {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                ctx.notify();
                return;
            }
        };
        self.busy = true;
        self.error = None;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let history_retention = crate::settings::SshSettings::as_ref(ctx)
            .persistent_history_retention
            .value()
            .to_proto() as i32;
        ctx.notify();
        ctx.spawn(
            async move {
                let response = checked(
                    client
                        .persistent_workspace(PersistentWorkspaceRequest {
                            protocol_version: 1,
                            operation: PersistentWorkspaceOperation::Create as i32,
                            workspace_id: id.clone(),
                            bootstrap: Some(profile),
                            history_retention: Some(history_retention),
                            ..Default::default()
                        })
                        .await?,
                )?;
                let workspace = response
                    .workspaces
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("Missing created workspace"))?;
                if workspace.workspace_id != id {
                    bail!("Create response returned a different workspace");
                }
                Ok(workspace)
            },
            |me, result: Result<PersistentWorkspace>, ctx| {
                me.busy = false;
                match result {
                    Ok(workspace) => me.attach(workspace, ctx),
                    Err(error) => {
                        me.error = Some(error.to_string());
                        ctx.notify();
                    }
                }
            },
        );
    }

    fn attach(&mut self, workspace: PersistentWorkspace, ctx: &mut ViewContext<Self>) {
        let shell_type = match workspace.shell.as_deref() {
            Some("bash") => ShellType::Bash,
            Some("zsh") => ShellType::Zsh,
            Some("fish") => ShellType::Fish,
            _ => {
                self.error = Some("This pane was not created with durable Warp shell integration. It has not been modified.".into());
                ctx.notify();
                return;
            }
        };
        let Some(connection) = self.connection.clone() else {
            return;
        };
        if connection.current_workspace.as_ref() == Some(&WorkspaceKey::from(&workspace)) {
            ctx.emit(PickerEvent::Close);
            return;
        }
        let owner = if let Some(owner) = connection.owner {
            owner
        } else {
            let Some(recipe) = connection.recipe else {
                self.error = Some("Reconnect details are unavailable for this SSH connection. Open a new SSH connection from this custom build first.".into());
                ctx.notify();
                return;
            };
            match crate::terminal::persistent_tty::connection_owner::ConnectionOwner::create(
                recipe, ctx,
            ) {
                Ok(owner) => owner,
                Err(error) => {
                    self.error = Some(format!("Cannot own this connection: {error:#}"));
                    ctx.notify();
                    return;
                }
            }
        };
        let Some(view) = ctx.view_with_id::<TerminalView>(ctx.window_id(), self.terminal_view_id)
        else {
            return;
        };
        let attachment = WorkspaceAttachment {
            workspace_id: workspace.workspace_id,
            generation: workspace.generation,
            shell_type,
            control_path: owner.as_ref(ctx).socket_path(),
            connection: owner.as_ref(ctx).slot(),
            owner,
        };
        view.update(ctx, |_, ctx| {
            ctx.emit(crate::terminal::view::Event::OpenPersistentWorkspace(
                attachment,
            ))
        });
        ctx.emit(PickerEvent::Close);
    }

    fn terminate(&mut self, key: &WorkspaceKey, ctx: &mut ViewContext<Self>) {
        if self.busy {
            return;
        }
        let Some(workspace) = find_workspace(&self.workspaces, key).cloned() else {
            self.error =
                Some("That workspace changed. Refresh the list before terminating it.".into());
            ctx.notify();
            return;
        };
        let identity = (workspace.workspace_id.clone(), workspace.generation.clone());
        if self.confirm_termination.as_ref() != Some(&identity) {
            self.confirm_termination = Some(identity);
            ctx.notify();
            return;
        }
        let client = match self.client() {
            Ok(client) => client,
            Err(error) => {
                self.error = Some(error.to_string());
                ctx.notify();
                return;
            }
        };
        self.busy = true;
        self.error = None;
        let terminating_current = self
            .connection
            .as_ref()
            .and_then(|connection| connection.current_workspace.as_ref())
            == Some(key);
        ctx.notify();
        ctx.spawn(
            async move {
                checked(
                    client
                        .persistent_workspace(PersistentWorkspaceRequest {
                            protocol_version: 1,
                            operation: PersistentWorkspaceOperation::Terminate as i32,
                            workspace_id: workspace.workspace_id,
                            generation: workspace.generation,
                            ..Default::default()
                        })
                        .await?,
                )
            },
            move |me, result: Result<PersistentWorkspaceResponse>, ctx| {
                me.busy = false;
                me.confirm_termination = None;
                match result {
                    Ok(_) if terminating_current => {
                        // This is an explicit, confirmed remote termination. Close
                        // the corresponding local pane only after acknowledgement;
                        // ordinary tab close continues to detach without killing.
                        if let Some(view) =
                            ctx.view_with_id::<TerminalView>(ctx.window_id(), me.terminal_view_id)
                        {
                            view.update(ctx, |_, ctx| {
                                ctx.emit(crate::terminal::view::Event::Exited)
                            });
                        }
                        ctx.emit(PickerEvent::Close);
                    }
                    Ok(_) => me.refresh(ctx),
                    Err(error) => {
                        me.error = Some(error.to_string());
                        ctx.notify();
                    }
                }
            },
        );
    }
}

fn checked(response: PersistentWorkspaceResponse) -> Result<PersistentWorkspaceResponse> {
    if response.protocol_version != 1
        || !response.management_supported
        || !response.terminal_transport_supported
        || !response.block_replay_supported
    {
        bail!(
            "This SSH extension does not support persistent workspaces. Install the matching custom build."
        );
    }
    if let Some(error) = &response.error {
        bail!("{}: {}", error.code, error.message);
    }
    Ok(response)
}

impl Entity for PersistentWorkspacePopup {
    type Event = PickerEvent;
}

impl TypedActionView for PersistentWorkspacePopup {
    type Action = Action;
    fn handle_action(&mut self, action: &Action, ctx: &mut ViewContext<Self>) {
        match action {
            Action::Close => {
                self.confirm_termination = None;
                ctx.emit(PickerEvent::Close);
            }
            Action::Refresh => self.refresh(ctx),
            Action::New => self.create(ctx),
            Action::Terminate(key) => self.terminate(key, ctx),
            Action::Select(key) if !self.busy => {
                if let Some(workspace) = find_workspace(&self.workspaces, key).cloned() {
                    self.attach(workspace, ctx);
                }
            }
            Action::Select(_) => {}
            Action::Up => {
                self.selected = self.selected.saturating_sub(1);
                ctx.notify();
            }
            Action::Down => {
                self.selected = (self.selected + 1).min(self.workspaces.len());
                ctx.notify();
            }
            Action::Enter if self.selected == self.workspaces.len() => self.create(ctx),
            Action::Enter => {
                if let Some(workspace) = self.workspaces.get(self.selected) {
                    self.handle_action(&Action::Select(WorkspaceKey::from(workspace)), ctx);
                }
            }
        }
    }
}

impl View for PersistentWorkspacePopup {
    fn ui_name() -> &'static str {
        "PersistentWorkspacePopup"
    }
    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let background = theme.surface_2();
        let foreground = blended_colors::text_main(theme, background);
        let font = appearance.ui_font_family();
        let size = appearance.ui_font_size();
        let label = |value: String| {
            Text::new(value, font, size)
                .with_color(foreground)
                .soft_wrap(true)
                .finish()
        };
        let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        column.add_child(
            Container::new(label("Persistent workspaces".into()))
                .with_horizontal_padding(10.)
                .with_vertical_padding(10.)
                .finish(),
        );
        if let Some(error) = &self.error {
            column.add_child(
                Container::new(label(error.clone()))
                    .with_horizontal_padding(10.)
                    .with_vertical_padding(10.)
                    .finish(),
            );
        }
        if self.confirm_termination.is_some() {
            column.add_child(Container::new(label("Stops this shell and its job and deletes retained history. Click its red x again to delete.".into()))
                .with_horizontal_padding(10.).with_vertical_padding(10.).finish());
        }
        for (index, workspace) in self.workspaces.iter().enumerate() {
            let key = WorkspaceKey::from(workspace);
            let select_key = key.clone();
            let current = self
                .connection
                .as_ref()
                .and_then(|c| c.current_workspace.as_ref())
                == Some(&key);
            let prefix = if current { "Current: " } else { "" };
            let status = workspace_status(workspace);
            let title = format!(
                "{prefix}{} {} | {status}\n{}",
                workspace.shell.as_deref().unwrap_or("legacy"),
                workspace.window_id,
                history_label(workspace.history_storage_bytes)
            );
            let selected = self.selected == index;
            let row = Hoverable::new(self.row_mouse[index].0.clone(), move |state| {
                let text = Text::new(title.clone(), font, size)
                    .with_color(foreground)
                    .soft_wrap(true)
                    .finish();
                let mut container = Container::new(text)
                    .with_horizontal_padding(10.)
                    .with_vertical_padding(10.);
                if state.is_hovered() || selected {
                    container = container.with_background(theme.surface_3());
                }
                container.finish()
            })
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(Action::Select(select_key.clone()))
            })
            .with_cursor(Cursor::PointingHand)
            .finish();
            let cross = Hoverable::new(self.row_mouse[index].1.clone(), move |_| {
                Container::new(
                    Text::new("x", font, size + 2.)
                        .with_color(theme.ansi_fg_red())
                        .finish(),
                )
                .with_horizontal_padding(10.)
                .with_vertical_padding(10.)
                .finish()
            })
            .on_click(move |ctx, _, _| ctx.dispatch_typed_action(Action::Terminate(key.clone())))
            .with_cursor(Cursor::PointingHand)
            .finish();
            column.add_child(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_child(Expanded::new(1., row).finish())
                    .with_child(cross)
                    .finish(),
            );
        }
        let new_label = if self.busy {
            "Working..."
        } else {
            "+ New workspace"
        };
        column.add_child(
            Hoverable::new(self.new_mouse.clone(), move |_| {
                Container::new(label(new_label.into()))
                    .with_horizontal_padding(10.)
                    .with_vertical_padding(10.)
                    .finish()
            })
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(Action::New))
            .with_cursor(Cursor::PointingHand)
            .finish(),
        );
        column.add_child(
            Hoverable::new(self.refresh_mouse.clone(), move |_| {
                Container::new(label("Refresh".into()))
                    .with_horizontal_padding(10.)
                    .with_vertical_padding(10.)
                    .finish()
            })
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(Action::Refresh))
            .with_cursor(Cursor::PointingHand)
            .finish(),
        );
        let scroll = ClippedScrollable::vertical(
            self.scroll.clone(),
            column.finish(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            warpui::elements::Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish();
        Dismiss::new(
            ConstrainedBox::new(
                Container::new(scroll)
                    .with_background(background)
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
                    .with_drop_shadow(DropShadow::default())
                    .finish(),
            )
            .with_width(390.)
            .with_max_height(360.)
            .finish(),
        )
        .prevent_interaction_with_other_elements()
        .on_dismiss(|ctx, _| ctx.dispatch_typed_action(Action::Close))
        .finish()
    }
}

#[cfg(test)]
#[path = "persistent_workspace_popup_tests.rs"]
mod tests;
