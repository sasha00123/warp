use super::*;

#[test]
fn persistent_history_defaults_to_retention_until_explicit_deletion() {
    assert_eq!(PersistentHistoryRetention::default(), PersistentHistoryRetention::UntilWorkspaceDeleted);
    assert_eq!(PersistentHistoryRetention::default().to_proto(),
        remote_server::proto::PersistentHistoryRetention::UntilWorkspaceDeleted);
}

#[test]
fn persistent_history_configuration_round_trips_both_policies() {
    for (policy, serialized) in [
        (PersistentHistoryRetention::UntilWorkspaceDeleted, "\"until_workspace_deleted\""),
        (PersistentHistoryRetention::Rolling, "\"rolling\""),
    ] {
        assert_eq!(serde_json::to_string(&policy).unwrap(), serialized);
        assert_eq!(serde_json::from_str::<PersistentHistoryRetention>(serialized).unwrap(), policy);
    }
    assert!(serde_json::from_str::<PersistentHistoryRetention>("\"unknown\"").is_err());
}
