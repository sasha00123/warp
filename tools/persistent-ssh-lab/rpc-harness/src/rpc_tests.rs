use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message;

use crate::persistent_workspace_rpc::handle;
use crate::proto::{
    ClientMessage, PersistentWorkspaceOperation as Op, PersistentWorkspaceRequest,
    PersistentWorkspaceResponse, SessionScopedRequest, client_message, session_scoped_request,
};

fn request(operation: Op) -> PersistentWorkspaceRequest {
    PersistentWorkspaceRequest {
        protocol_version: 1,
        operation: operation.into(),
        ..Default::default()
    }
}

#[test]
fn an_old_or_future_protocol_cannot_create_workspaces() {
    for version in [0, 2, u32::MAX] {
        let response = handle(PersistentWorkspaceRequest {
            protocol_version: version,
            ..request(Op::Create)
        });
        assert_eq!(response.error.unwrap().code, "unsupported_version");
        assert!(response.workspaces.is_empty());
    }
}

#[test]
fn rejects_unknown_operations_and_ambiguous_parameters() {
    let response = handle(PersistentWorkspaceRequest {
        operation: 99,
        ..request(Op::Probe)
    });
    assert_eq!(response.error.unwrap().code, "invalid_request");
    for operation in [Op::Probe, Op::List] {
        let response = handle(PersistentWorkspaceRequest {
            workspace_id: "unexpected".into(),
            ..request(operation)
        });
        assert_eq!(response.error.unwrap().code, "invalid_request");
    }
    let response = handle(PersistentWorkspaceRequest {
        generation: "unexpected".into(),
        ..request(Op::Create)
    });
    assert_eq!(response.error.unwrap().code, "invalid_request");
}

#[test]
fn retention_is_only_accepted_for_initialized_creation() {
    for (operation, policy, bootstrap) in [
        (Op::List, 0, None),
        (Op::Create, 1, None),
        (Op::Create, 99, Some(crate::proto::PersistentShellBootstrap::default())),
    ] {
        let response = handle(PersistentWorkspaceRequest {
            history_retention: Some(policy), bootstrap, ..request(operation)
        });
        assert_eq!(response.error.unwrap().code, "invalid_request");
    }
}

#[test]
fn terminal_requests_reject_unknown_or_cross_operation_parameters() {
    use crate::proto::PersistentTerminalRequest;
    for terminal in [
        None,
        Some(PersistentTerminalRequest {
            operation: 99,
            ..Default::default()
        }),
        Some(PersistentTerminalRequest {
            operation: 0,
            input: vec![3],
            ..Default::default()
        }),
        Some(PersistentTerminalRequest {
            operation: 1,
            columns: 80,
            ..Default::default()
        }),
        Some(PersistentTerminalRequest {
            operation: 2,
            cursor: 7,
            ..Default::default()
        }),
    ] {
        let response = handle(PersistentWorkspaceRequest {
            terminal,
            ..request(Op::Terminal)
        });
        assert_eq!(response.error.unwrap().code, "invalid_request");
    }
    let response = handle(PersistentWorkspaceRequest {
        terminal: Some(PersistentTerminalRequest::default()),
        ..request(Op::List)
    });
    assert_eq!(response.error.unwrap().code, "invalid_request");
}

#[test]
fn protobuf_envelope_preserves_workspace_request() {
    let original = ClientMessage {
        request_id: "request-1".into(),
        message: Some(client_message::Message::SessionScoped(
            SessionScopedRequest {
                message: Some(session_scoped_request::Message::PersistentWorkspace(
                    PersistentWorkspaceRequest {
                        workspace_id: "0123456789abcdef0123456789abcdef".into(),
                        working_directory: Some("/tmp/a directory;not-a-command".into()),
                        ..request(Op::Create)
                    },
                )),
            },
        )),
    };
    assert_eq!(
        ClientMessage::decode(original.encode_to_vec().as_slice()).unwrap(),
        original
    );
}

#[test]
fn native_transport_and_block_replay_capabilities_are_advertised() {
    let response = handle(request(Op::Probe));
    assert!(response.terminal_transport_supported);
    assert!(response.block_replay_supported);
    assert_eq!(
        PersistentWorkspaceResponse::decode(response.encode_to_vec().as_slice()).unwrap(),
        response
    );
}

#[test]
#[ignore = "requires an isolated Unix host with tmux"]
fn rpc_create_retry_resolve_list_and_terminate() {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let id = format!(
        "{:032x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            + SEQUENCE.fetch_add(1, Ordering::Relaxed) as u128
    );
    let create = PersistentWorkspaceRequest {
        workspace_id: id.clone(),
        ..request(Op::Create)
    };
    let first = handle(create.clone());
    assert!(first.error.is_none(), "{:?}", first.error);
    let workspace = first.workspaces[0].clone();
    struct Cleanup(String, String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            handle(PersistentWorkspaceRequest {
                workspace_id: self.0.clone(),
                generation: self.1.clone(),
                ..request(Op::Terminate)
            });
        }
    }
    let cleanup = Cleanup(id.clone(), workspace.generation.clone());
    assert_eq!(handle(create).workspaces, first.workspaces);
    let resolved = handle(PersistentWorkspaceRequest {
        workspace_id: id.clone(),
        generation: workspace.generation.clone(),
        ..request(Op::Resolve)
    });
    assert_eq!(resolved.workspaces, first.workspaces);
    let stale = handle(PersistentWorkspaceRequest {
        workspace_id: id.clone(),
        generation: "0".repeat(32),
        ..request(Op::Terminate)
    });
    assert_eq!(stale.error.unwrap().code, "stale_workspace");
    assert!(
        handle(request(Op::List))
            .workspaces
            .iter()
            .any(|entry| entry.workspace_id == id)
    );
    drop(cleanup);
    let deleted = handle(PersistentWorkspaceRequest {
        workspace_id: id,
        generation: workspace.generation,
        ..request(Op::Resolve)
    });
    assert_eq!(deleted.error.unwrap().code, "workspace_not_found");
}
