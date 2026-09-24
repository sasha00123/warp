use super::*;

fn page(start: u64, bytes: &[u8], end: u64) -> PersistentTerminalOutput {
    PersistentTerminalOutput {
        start_cursor: start,
        next_cursor: start + bytes.len() as u64,
        high_watermark: end,
        output: bytes.into(),
        recorder_connected: true,
        ..Default::default()
    }
}

#[test]
fn cursor_is_committed_only_after_terminal_model_consumes_bytes() {
    let mut state = ReplayState::new(0);
    state.connected();
    let ticket = state.read_ticket().unwrap();
    let batch = state.prepare(ticket, page(0, b"abc", 3)).unwrap();
    assert_eq!(state.cursor(), 0);
    assert!(!state.can_send_input());
    assert_eq!(batch.historical_bytes, 3);
    state.commit(batch).unwrap();
    assert_eq!(state.cursor(), 3);
    assert!(state.can_send_input());
    assert_eq!(
        state.prepare(ticket, page(0, b"abc", 3)).unwrap_err(),
        ReplayError::StaleResponse
    );
}

#[test]
fn catch_up_boundary_is_fixed_even_as_new_output_arrives() {
    let mut state = ReplayState::new(0);
    state.connected();
    let first = state
        .prepare(state.read_ticket().unwrap(), page(0, b"abc", 5))
        .unwrap();
    state.commit(first).unwrap();
    assert_eq!(state.phase(), ReplayPhase::Replaying);
    let second = state
        .prepare(state.read_ticket().unwrap(), page(3, b"deLIVE", 9))
        .unwrap();
    assert_eq!(second.historical_bytes, 2);
    state.commit(second).unwrap();
    assert_eq!(state.phase(), ReplayPhase::Live);
}

#[test]
fn reconnect_discards_late_responses_and_never_retries_uncertain_input() {
    let mut state = ReplayState::new(0);
    state.connected();
    let old = state.read_ticket().unwrap();
    state.disconnected(true);
    state.connected();
    assert_eq!(
        state.prepare(old, page(0, b"old", 3)).unwrap_err(),
        ReplayError::StaleResponse
    );
    let batch = state
        .prepare(state.read_ticket().unwrap(), page(0, b"new", 3))
        .unwrap();
    state.commit(batch).unwrap();
    assert!(!state.can_send_input());
    assert!(state.input_delivery_unknown());
    state.acknowledge_input_uncertainty();
    assert!(state.can_send_input());
}

#[test]
fn switching_to_another_view_rejects_the_previous_views_response() {
    let mut first = ReplayState::new(0);
    let mut second = ReplayState::new(0);
    first.connected();
    second.connected();
    assert_eq!(
        second
            .prepare(first.read_ticket().unwrap(), page(0, b"first", 5))
            .unwrap_err(),
        ReplayError::StaleResponse
    );
}

#[test]
fn malformed_and_gapped_output_do_not_advance_the_model_cursor() {
    let mut state = ReplayState::new(0);
    state.connected();
    let ticket = state.read_ticket().unwrap();
    assert_eq!(
        state.prepare(ticket, page(1, b"bad", 4)).unwrap_err(),
        ReplayError::InvalidPage
    );
    let mut gap = page(10, b"remaining", 19);
    gap.earliest_cursor = 10;
    gap.history_gap = true;
    assert_eq!(
        state.prepare(ticket, gap).unwrap_err(),
        ReplayError::HistoryGap {
            earliest_cursor: 10
        }
    );
    assert_eq!(state.cursor(), 0);
    assert!(!state.can_send_input());
    state.reset_after_reported_gap().unwrap();
    assert_eq!(state.cursor(), 10);
    assert_eq!(
        state.prepare(ticket, page(0, b"stale", 5)).unwrap_err(),
        ReplayError::StaleResponse
    );
}

#[test]
fn missing_recorder_and_eof_are_not_a_live_shell() {
    for closed in [false, true] {
        let mut state = ReplayState::new(0);
        state.connected();
        let mut output = page(0, b"final", 5);
        output.recorder_connected = false;
        output.closed = closed;
        let batch = state.prepare(state.read_ticket().unwrap(), output).unwrap();
        state.commit(batch).unwrap();
        assert_eq!(
            state.phase(),
            if closed {
                ReplayPhase::Closed
            } else {
                ReplayPhase::Disconnected
            }
        );
        assert!(!state.can_send_input());
    }
}
