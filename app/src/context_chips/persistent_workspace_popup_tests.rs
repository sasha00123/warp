use super::*;

fn workspace(id: &str, generation: &str) -> PersistentWorkspace {
    PersistentWorkspace {
        workspace_id: id.into(),
        generation: generation.into(),
        ..Default::default()
    }
}

#[test]
fn persistent_picker_actions_survive_reordering_without_retargeting() {
    let original = workspace("job-a", "first");
    let key = WorkspaceKey::from(&original);
    let refreshed = vec![workspace("job-b", "other"), original];
    assert_eq!(
        find_workspace(&refreshed, &key).unwrap().workspace_id,
        "job-a"
    );
}

#[test]
fn persistent_picker_rejects_a_recreated_or_removed_target() {
    let key = WorkspaceKey::from(&workspace("job-a", "first"));
    assert!(find_workspace(&[workspace("job-a", "second")], &key).is_none());
    assert!(find_workspace(&[], &key).is_none());
}

#[test]
fn persistent_picker_surfaces_extension_errors_and_version_mismatch() {
    assert!(checked(PersistentWorkspaceResponse::default()).is_err());
    let mut response = PersistentWorkspaceResponse {
        protocol_version: 1,
        management_supported: true,
        ..Default::default()
    };
    assert!(
        checked(response.clone()).is_err(),
        "Management-only extensions cannot restore native blocks"
    );
    response.terminal_transport_supported = true;
    assert!(
        checked(response.clone()).is_err(),
        "Live output without replay is insufficient"
    );
    response.block_replay_supported = true;
    assert!(checked(response.clone()).is_ok());
    response.error = Some(remote_server::proto::PersistentWorkspaceError {
        code: "stale".into(),
        message: "Workspace was replaced".into(),
    });
    assert!(checked(response).unwrap_err().to_string().contains("stale"));
}

#[test]
fn persistent_picker_status_uses_shell_hooks_and_exit_takes_precedence() {
    let mut item = workspace("job", "incarnation");
    assert_eq!(workspace_status(&item), "status unknown");
    for status in ["starting", "idle", "running"] {
        item.activity = Some(status.into());
        assert_eq!(workspace_status(&item), status);
    }
    item.exited = true;
    assert_eq!(workspace_status(&item), "exited");
}

#[test]
fn persistent_history_size_does_not_report_unknown_as_zero() {
    assert!(history_label(None).contains("unavailable"));
    assert!(history_label(Some(0)).contains("0.0 MiB"));
    assert!(history_label(Some(1024 * 1024 * 1024)).contains("Delete unused workspaces"));
}
