use std::path::Path;

use crate::persistent_workspace::{Backend, SOCKET, Workspace};
use crate::proto::{
    PersistentWorkspace, PersistentWorkspaceError, PersistentWorkspaceOperation,
    PersistentWorkspaceRequest, PersistentWorkspaceResponse,
};

pub const PROTOCOL_VERSION: u32 = 1;

pub fn handle(request: PersistentWorkspaceRequest) -> PersistentWorkspaceResponse {
    let mut response = PersistentWorkspaceResponse {
        protocol_version: PROTOCOL_VERSION,
        management_supported: true,
        terminal_transport_supported: true,
        block_replay_supported: true,
        workspaces: Vec::new(),
        error: None,
        terminal_output: None,
    };
    let result = execute(request, &mut response);
    match result {
        Ok(workspaces) => response.workspaces = workspaces.into_iter().map(to_proto).collect(),
        Err(error) => response.error = Some(error),
    }
    response
}

fn execute(
    request: PersistentWorkspaceRequest,
    response: &mut PersistentWorkspaceResponse,
) -> Result<Vec<Workspace>, PersistentWorkspaceError> {
    let invalid = |message: &str| PersistentWorkspaceError {
        code: "invalid_request".into(),
        message: message.into(),
    };
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(PersistentWorkspaceError {
            code: "unsupported_version".into(),
            message: "Persistent-workspace protocol version 1 is required".into(),
        });
    }
    let operation = PersistentWorkspaceOperation::try_from(request.operation)
        .map_err(|_| invalid("Unknown workspace operation"))?;
    use PersistentWorkspaceOperation as Op;
    if operation != Op::Create && request.bootstrap.is_some() {
        return Err(invalid("Bootstrap assets are valid only on workspace creation"));
    }
    if let Some(policy) = request.history_retention
        && (operation != Op::Create || request.bootstrap.is_none()
            || crate::proto::PersistentHistoryRetention::try_from(policy).is_err())
    {
        return Err(invalid("History retention requires initialized workspace creation and a known policy"));
    }
    if (operation == Op::Terminal) != request.terminal.is_some() {
        return Err(invalid(
            "Terminal operation requires terminal parameters exclusively",
        ));
    }
    match operation {
        Op::Probe | Op::List
            if !request.workspace_id.is_empty()
                || !request.generation.is_empty()
                || request.working_directory.is_some() =>
        {
            return Err(invalid("Probe/list must not target a workspace"));
        }
        Op::Create if !request.generation.is_empty() => {
            return Err(invalid("Create must not supply a generation"));
        }
        Op::Resolve | Op::Terminate | Op::Terminal if request.working_directory.is_some() => {
            return Err(invalid(
                "Resolve/terminate cannot change the working directory",
            ));
        }
        Op::Probe | Op::List | Op::Create | Op::Resolve | Op::Terminate | Op::Terminal => {}
    }
    let map_error = |error: crate::persistent_workspace::Error| PersistentWorkspaceError {
        code: error.kind.code().into(),
        message: error.message,
    };
    let backend = Backend::new(SOCKET).map_err(map_error)?;
    if operation == Op::Terminal {
        response.terminal_output = terminal(&backend, &request).map_err(map_error)?;
        return backend
            .resolve(&request.workspace_id, &request.generation)
            .map(|workspace| vec![workspace])
            .map_err(map_error);
    }
    match operation {
        Op::Probe => backend.probe().map(|()| Vec::new()),
        Op::Create => create_workspace(&backend, &request).map(|workspace| vec![workspace]),
        Op::List => backend.list(),
        Op::Resolve => backend
            .resolve(&request.workspace_id, &request.generation)
            .map(|workspace| vec![workspace]),
        Op::Terminate => backend
            .terminate(&request.workspace_id, &request.generation)
            .and_then(|()| {
                if let Some(home) = std::env::var_os("HOME") {
                    let root = std::path::PathBuf::from(home).join(".local/state/warp/persistent-output-v1");
                    backend.delete_retained_history(&request.workspace_id, &request.generation, &root)?;
                }
                Ok(())
            })
            .map(|()| Vec::new()),
        Op::Terminal => unreachable!("Terminal operation handled above"),
    }
    .map_err(map_error)
}

fn terminal(
    backend: &Backend,
    request: &PersistentWorkspaceRequest,
) -> Result<Option<crate::proto::PersistentTerminalOutput>, crate::persistent_workspace::Error> {
    use crate::persistent_workspace::{Error, ErrorKind};
    use crate::proto::persistent_terminal_request::Operation as Op;
    let invalid = |message: &str| Error {
        kind: ErrorKind::InvalidRequest,
        message: message.into(),
    };
    let params = request
        .terminal
        .as_ref()
        .ok_or_else(|| invalid("Missing terminal parameters"))?;
    let operation =
        Op::try_from(params.operation).map_err(|_| invalid("Unknown terminal operation"))?;
    if (operation != Op::Input && !params.input.is_empty())
        || (operation != Op::Resize && (params.columns != 0 || params.rows != 0))
        || (matches!(operation, Op::Input | Op::Resize) && params.cursor != 0)
    {
        return Err(invalid("Unexpected parameters for terminal operation"));
    }
    let id = &request.workspace_id;
    let generation = &request.generation;
    match operation {
        Op::Input => {
            backend.send_input(id, generation, &params.input)?;
            return Ok(None);
        }
        Op::Resize => {
            backend.resize(id, generation, params.columns, params.rows)?;
            return Ok(None);
        }
        Op::Attach | Op::Read => {}
    }
    let home = std::env::var_os("HOME").ok_or_else(|| invalid("Remote HOME is not set"))?;
    let root = std::path::PathBuf::from(home).join(".local/state/warp/persistent-output-v1");
    if operation == Op::Attach {
        let executable = std::env::current_exe().map_err(|error| Error {
            kind: ErrorKind::Failed,
            message: error.to_string(),
        })?;
        backend.start_recording(id, generation, &executable, &root)?;
    }
    let page = backend.read_output(id, generation, &root, params.cursor)?;
    Ok(Some(crate::proto::PersistentTerminalOutput {
        start_cursor: page.start_cursor,
        next_cursor: page.next_cursor,
        earliest_cursor: page.earliest_cursor,
        high_watermark: page.high_watermark,
        history_gap: page.history_gap,
        closed: page.closed,
        output: page.bytes,
        recorder_connected: backend.recorder_connected(id, generation)?,
    }))
}

fn to_proto(workspace: Workspace) -> PersistentWorkspace {
    let history_storage_bytes = std::env::var_os("HOME").and_then(|home| {
        let journal = std::path::PathBuf::from(home).join(".local/state/warp/persistent-output-v1")
            .join(format!("{}-{}", workspace.id, workspace.generation));
        crate::persistent_journal::retained_storage_bytes(&journal).ok()
    });
    let shell = crate::persistent_shell::stored_shell(&workspace.id, &workspace.generation);
    let activity = std::env::var_os("HOME").and_then(|home| {
        crate::persistent_shell::activity(
            &std::path::PathBuf::from(home).join(".local/state/warp/persistent-output-v1"),
            &workspace.id, &workspace.generation,
        ).map(str::to_owned)
    });
    PersistentWorkspace {
        shell,
        activity,
        workspace_id: workspace.id,
        generation: workspace.generation,
        session_id: workspace.session_id,
        window_id: workspace.window_id,
        pane_id: workspace.pane_id,
        shell_pid: workspace.shell_pid,
        exited: workspace.exited,
        exit_code: workspace.exit_code,
        history_storage_bytes,
    }
}

fn create_workspace(
    backend: &Backend,
    request: &PersistentWorkspaceRequest,
) -> Result<Workspace, crate::persistent_workspace::Error> {
    let directory = request.working_directory.as_deref().map(Path::new);
    let Some(profile) = &request.bootstrap else {
        return backend.create(&request.workspace_id, directory);
    };
    use crate::persistent_workspace::{BootstrapProfile, Error, ErrorKind};
    let executable = std::env::current_exe().map_err(|error| Error {
        kind: ErrorKind::Failed, message: error.to_string(),
    })?;
    let home = std::env::var_os("HOME").ok_or_else(|| Error {
        kind: ErrorKind::Failed, message: "Remote HOME is not set".into(),
    })?;
    let root = std::path::PathBuf::from(home).join(".local/state/warp/persistent-output-v1");
    let retention = match request.history_retention.and_then(|value|
        crate::proto::PersistentHistoryRetention::try_from(value).ok()) {
        Some(crate::proto::PersistentHistoryRetention::Rolling) => crate::persistent_journal::RetentionPolicy::Rolling,
        _ => crate::persistent_journal::RetentionPolicy::UntilWorkspaceDeleted,
    };
    backend.create_initialized_with_retention(&request.workspace_id, directory, BootstrapProfile {
        shell: &profile.shell, init_script: &profile.init_script,
        bootstrap_script: &profile.bootstrap_script,
    }, &executable, &root, retention)
}
