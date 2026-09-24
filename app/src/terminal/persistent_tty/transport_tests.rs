use super::*;

#[test]
fn persistent_input_is_rejected_without_destroying_the_terminal() {
    let (handle, _rx) = TransportHandle::channel();
    for phase in [TransportPhase::Disconnected, TransportPhase::Replaying, TransportPhase::Exited,
        TransportPhase::HistoryGap { earliest_cursor: 1024 }, TransportPhase::Failed("recorder stopped".into())] {
        handle.update(3, phase);
        assert!(matches!(handle.send(Message::Input(vec![3].into())), Err(EventLoopSendError::Other(_))));
    }
}

#[test]
fn persistent_input_keeps_its_connection_epoch() {
    let (handle, rx) = TransportHandle::channel();
    handle.update(3, TransportPhase::Live);
    handle.send(Message::Input(b"run-once\r".to_vec().into())).unwrap();
    handle.update(4, TransportPhase::Live);
    assert_eq!(rx.try_recv().unwrap().epoch, 3);
}

#[test]
fn persistent_uncertain_input_requires_explicit_acknowledgement() {
    let (handle, rx) = TransportHandle::channel();
    handle.status.lock().input_delivery_unknown = true;
    handle.update(8, TransportPhase::Live);
    assert!(handle.send(Message::Input(vec![3].into())).is_err());
    handle.acknowledge_uncertain_input();
    handle.send(Message::Input(vec![3].into())).unwrap();
    assert_eq!(rx.try_recv().unwrap().epoch, 8);
}

#[test]
fn persistent_shutdown_pauses_for_undo_close_without_sending_remote_input() {
    let (handle, rx) = TransportHandle::channel();
    handle.update(1, TransportPhase::Live);
    handle.send(Message::Shutdown).unwrap();
    assert!(!handle.stopped.load(Ordering::Acquire));
    assert!(!rx.is_closed());
    assert!(handle.paused.load(Ordering::Acquire));
    assert!(rx.try_recv().is_err());
    assert!(handle.send(Message::Input(b"exit\r".to_vec().into())).is_err());
    handle.resume();
    assert!(!handle.accepts_input()); // Catch up before accepting new input.
    handle.update(1, TransportPhase::Live);
    assert!(handle.accepts_input());
}

#[test]
fn persistent_reopen_invalidates_pending_input_even_on_the_same_connection() {
    let (handle, rx) = TransportHandle::channel();
    handle.update(1, TransportPhase::Live);
    handle.send(Message::Input(b"do-not-retry\r".to_vec().into())).unwrap();
    handle.pause();
    handle.status.lock().input_delivery_unknown = true;
    handle.resume();
    handle.update(1, TransportPhase::Live);
    let queued = rx.try_recv().unwrap();
    assert_ne!(queued.lifecycle, handle.lifecycle.load(Ordering::Acquire));
    assert!(handle.status().input_delivery_unknown);
    assert!(!handle.accepts_input());
}

#[test]
fn persistent_final_disposal_cannot_be_revived_by_undo_close() {
    let (handle, rx) = TransportHandle::channel();
    handle.detach();
    handle.resume();
    handle.update(1, TransportPhase::Live);
    assert!(handle.stopped.load(Ordering::Acquire));
    assert!(rx.is_closed());
    assert!(!handle.accepts_input());
}

#[test]
fn persistent_recorder_loss_is_a_failure_not_an_endless_attach() {
    for phase in [ReplayPhase::Disconnected, ReplayPhase::Closed] {
        assert!(matches!(phase_after_replay(phase, true, false), TransportPhase::Failed(_)));
    }
    assert_eq!(phase_after_replay(ReplayPhase::Closed, true, true), TransportPhase::Exited);
    assert_eq!(phase_after_replay(ReplayPhase::Live, true, true), TransportPhase::Replaying);
    assert_eq!(phase_after_replay(ReplayPhase::Replaying, true, true), TransportPhase::Replaying);
}

#[test]
fn persistent_shell_exit_waits_for_the_recorders_final_page() {
    for shell_ready in [false, true] {
        for phase in [ReplayPhase::Replaying, ReplayPhase::Live] {
            assert_eq!(phase_after_replay(phase, shell_ready, true), TransportPhase::Replaying);
        }
        assert_eq!(phase_after_replay(ReplayPhase::Closed, shell_ready, true), TransportPhase::Exited);
    }
}

#[test]
fn persistent_history_gap_retains_its_identity_and_never_enables_input() {
    let phase = phase_after_replay(ReplayPhase::HistoryGap { earliest_cursor: 4096 }, true, false);
    assert_eq!(phase, TransportPhase::HistoryGap { earliest_cursor: 4096 });
    let (handle, _) = TransportHandle::channel();
    handle.update(1, phase);
    handle.acknowledge_uncertain_input();
    assert!(!handle.accepts_input());
}
