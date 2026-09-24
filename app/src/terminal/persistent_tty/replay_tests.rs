use super::*;

fn adapter() -> (NativeReplay, async_channel::Receiver<Event>) {
    let (wake_tx, _wake_rx) = async_channel::unbounded();
    let (event_tx, event_rx) = async_channel::unbounded();
    let (bytes_tx, _bytes_rx) = async_broadcast::broadcast(1);
    let events = ChannelEventListener::new(wake_tx, event_tx, bytes_tx);
    let model = Arc::new(FairMutex::new(TerminalModel::mock(None, None)));
    (NativeReplay::new(model, events), event_rx)
}

fn page(start: u64, bytes: &[u8]) -> PersistentTerminalOutput {
    PersistentTerminalOutput {
        start_cursor: start,
        next_cursor: start + bytes.len() as u64,
        earliest_cursor: 0,
        high_watermark: start + bytes.len() as u64,
        history_gap: false,
        closed: false,
        output: bytes.to_vec(),
        recorder_connected: true,
    }
}

#[test]
fn persistent_replay_suppresses_historical_queries_but_answers_live_queries() {
    let (mut replay, events) = adapter();
    replay.connected();
    let query = b"\x1b[c";
    let replies = replay
        .apply(replay.read_ticket().unwrap(), page(0, query))
        .unwrap();
    assert!(replies.is_empty());
    assert!(matches!(
        events.try_recv().unwrap(),
        Event::PersistentReplayState { replaying: true }
    ));
    assert!(matches!(
        events.try_recv().unwrap(),
        Event::PersistentReplayState { replaying: false }
    ));
    assert_eq!(replay.phase(), ReplayPhase::Live);
    let replies = replay
        .apply(
            replay.read_ticket().unwrap(),
            page(query.len() as u64, query),
        )
        .unwrap();
    assert!(
        !replies.is_empty(),
        "Live terminal queries must still be answered"
    );
}

#[test]
fn persistent_replay_rejects_a_previous_connection_before_mutating_the_model() {
    let (mut replay, events) = adapter();
    replay.connected();
    let old = replay.read_ticket().unwrap();
    replay.disconnected(false);
    replay.connected();
    assert_eq!(
        replay.apply(old, page(0, b"stale output")),
        Err(ReplayError::StaleResponse)
    );
    assert_eq!(replay.cursor(), 0);
    assert!(events.try_recv().is_err());
}

#[test]
fn persistent_replay_does_not_advance_past_a_retention_gap() {
    let (mut replay, _) = adapter();
    replay.connected();
    let mut retained = page(10, b"retained");
    retained.earliest_cursor = 10;
    retained.history_gap = true;
    assert_eq!(
        replay.apply(replay.read_ticket().unwrap(), retained),
        Err(ReplayError::HistoryGap {
            earliest_cursor: 10
        })
    );
    assert_eq!(replay.cursor(), 0);
    assert!(!replay.can_send_input());
}
