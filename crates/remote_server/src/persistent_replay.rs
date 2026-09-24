//! Client-side replay cursor, shared by the native terminal adapter and tests.
//!
//! Reading output never advances the cursor. Only committing bytes after the
//! terminal model consumes them does. Connection epochs reject late responses
//! from an old connection or a previously selected workspace.

use crate::proto::PersistentTerminalOutput;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayPhase {
    Disconnected,
    Replaying,
    Live,
    Closed,
    HistoryGap { earliest_cursor: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadTicket {
    epoch: u64,
    pub cursor: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReplayError {
    StaleResponse,
    InvalidPage,
    HistoryGap { earliest_cursor: u64 },
    NotConnected,
}

/// The adapter must parse `bytes[..historical_bytes]` with automatic shell
/// bootstrap and terminal-query replies disabled. The rest is live output.
#[derive(Debug)]
pub struct PreparedOutput {
    pub bytes: Vec<u8>,
    pub historical_bytes: usize,
    ticket: ReadTicket,
    next_cursor: u64,
    high_watermark: u64,
    closed: bool,
    recorder_connected: bool,
}

pub struct ReplayState {
    epoch: u64,
    cursor: u64,
    phase: ReplayPhase,
    replay_until: Option<u64>,
    input_delivery_unknown: bool,
}

fn epoch() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl ReplayState {
    /// A nonzero cursor is valid only alongside the exact terminal-model state
    /// persisted at that cursor. Rebuilding a model from scratch starts at zero.
    pub fn new(model_cursor: u64) -> Self {
        Self {
            epoch: epoch(),
            cursor: model_cursor,
            phase: ReplayPhase::Disconnected,
            replay_until: None,
            input_delivery_unknown: false,
        }
    }

    pub fn connected(&mut self) {
        self.epoch = epoch();
        self.phase = ReplayPhase::Replaying;
        self.replay_until = None;
    }

    pub fn disconnected(&mut self, input_in_flight: bool) {
        self.epoch = epoch();
        self.phase = ReplayPhase::Disconnected;
        self.input_delivery_unknown |= input_in_flight;
    }

    pub fn cursor(&self) -> u64 {
        self.cursor
    }
    pub fn phase(&self) -> ReplayPhase {
        self.phase
    }
    pub fn input_delivery_unknown(&self) -> bool {
        self.input_delivery_unknown
    }
    pub fn acknowledge_input_uncertainty(&mut self) {
        self.input_delivery_unknown = false;
    }
    pub fn can_send_input(&self) -> bool {
        self.phase == ReplayPhase::Live && !self.input_delivery_unknown
    }

    pub fn read_ticket(&self) -> Result<ReadTicket, ReplayError> {
        if !matches!(self.phase, ReplayPhase::Replaying | ReplayPhase::Live) {
            return Err(ReplayError::NotConnected);
        }
        Ok(ReadTicket {
            epoch: self.epoch,
            cursor: self.cursor,
        })
    }

    pub fn prepare(
        &mut self,
        ticket: ReadTicket,
        output: PersistentTerminalOutput,
    ) -> Result<PreparedOutput, ReplayError> {
        self.check(ticket)?;
        if output.start_cursor > output.next_cursor
            || output.next_cursor > output.high_watermark
            || output.earliest_cursor > output.start_cursor
            || output.output.len() > 256 * 1024
            || output.next_cursor - output.start_cursor != output.output.len() as u64
            || output.start_cursor != ticket.cursor.max(output.earliest_cursor)
            || output.history_gap != (ticket.cursor < output.earliest_cursor)
        {
            return Err(ReplayError::InvalidPage);
        }
        if output.history_gap {
            self.phase = ReplayPhase::HistoryGap {
                earliest_cursor: output.earliest_cursor,
            };
            return Err(ReplayError::HistoryGap {
                earliest_cursor: output.earliest_cursor,
            });
        }
        let until = *self.replay_until.get_or_insert(output.high_watermark);
        let historical_bytes = until
            .saturating_sub(output.start_cursor)
            .min(output.output.len() as u64) as usize;
        Ok(PreparedOutput {
            bytes: output.output,
            historical_bytes,
            ticket,
            next_cursor: output.next_cursor,
            high_watermark: output.high_watermark,
            closed: output.closed,
            recorder_connected: output.recorder_connected,
        })
    }

    /// Call only after parsing the whole batch successfully. A failed parse or
    /// disposed view leaves the durable cursor untouched so it can be replayed.
    pub fn commit(&mut self, output: PreparedOutput) -> Result<(), ReplayError> {
        self.check(output.ticket)?;
        self.cursor = output.next_cursor;
        self.phase = if output.closed && self.cursor == output.high_watermark {
            ReplayPhase::Closed
        } else if !output.recorder_connected && !output.closed {
            ReplayPhase::Disconnected
        } else if self.cursor < self.replay_until.unwrap_or(output.high_watermark) {
            ReplayPhase::Replaying
        } else {
            ReplayPhase::Live
        };
        Ok(())
    }

    /// The GUI must visibly report the gap and reset its parser/model before
    /// opting into the remaining bytes. A gap is never silently acknowledged.
    pub fn reset_after_reported_gap(&mut self) -> Result<(), ReplayError> {
        let ReplayPhase::HistoryGap { earliest_cursor } = self.phase else {
            return Err(ReplayError::InvalidPage);
        };
        self.cursor = earliest_cursor;
        self.connected();
        Ok(())
    }

    fn check(&self, ticket: ReadTicket) -> Result<(), ReplayError> {
        if ticket.epoch != self.epoch || ticket.cursor != self.cursor {
            Err(ReplayError::StaleResponse)
        } else if !matches!(self.phase, ReplayPhase::Replaying | ReplayPhase::Live) {
            Err(ReplayError::NotConnected)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "persistent_replay_tests.rs"]
mod tests;
