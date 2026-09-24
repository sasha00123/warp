use std::sync::Arc;
use parking_lot::FairMutex;
use remote_server::persistent_replay::{ReadTicket, ReplayError, ReplayPhase, ReplayState};
use remote_server::proto::PersistentTerminalOutput;

use crate::terminal::TerminalModel;
use crate::terminal::event::Event;
use crate::terminal::event_listener::ChannelEventListener;
use crate::terminal::model::ansi::Processor;

/// Applies an incarnation's ordered output to Warp's actual terminal model.
/// Network loss never calls TerminalModel::exit or completes the running block.
pub struct NativeReplay {
    model: Arc<FairMutex<TerminalModel>>,
    events: ChannelEventListener,
    processor: Processor,
    state: ReplayState,
    historical_events_open: bool,
}

impl NativeReplay {
    pub fn new(model: Arc<FairMutex<TerminalModel>>, events: ChannelEventListener) -> Self {
        Self { model, events, processor: Processor::new(), state: ReplayState::new(0),
            historical_events_open: false }
    }

    pub fn connected(&mut self) { self.state.connected(); }
    pub fn read_ticket(&self) -> Result<ReadTicket, ReplayError> { self.state.read_ticket() }
    pub fn phase(&self) -> ReplayPhase { self.state.phase() }
    pub fn cursor(&self) -> u64 { self.state.cursor() }
    pub fn can_send_input(&self) -> bool { self.state.can_send_input() }
    pub fn acknowledge_input_uncertainty(&mut self) { self.state.acknowledge_input_uncertainty(); }

    pub fn finish_shell(&self, exit_code: Option<i32>) {
        if let Some(exit_code) = exit_code {
            self.model.lock().finish_persistent_shell(exit_code);
        }
    }

    pub fn shell_ready(&self, session_id: warp_core::SessionId) -> bool {
        let model = self.model.lock();
        model.is_active_block_bootstrapped()
            && model.block_list().active_block().session_id() == Some(session_id)
    }

    pub fn disconnected(&mut self, input_delivery_unknown: bool) {
        self.state.disconnected(input_delivery_unknown);
        self.end_historical_events();
    }

    /// Returned terminal-query replies are live bytes only. The caller queues
    /// them through the same serialized writer as interactive input.
    pub fn apply(&mut self, ticket: ReadTicket, page: PersistentTerminalOutput) -> Result<Vec<u8>, ReplayError> {
        let batch = self.state.prepare(ticket, page)?;
        let mut replies = Vec::new();
        if batch.historical_bytes > 0 {
            if !self.historical_events_open {
                self.events.send_app_event(Event::PersistentReplayState { replaying: true });
                self.historical_events_open = true;
            }
            self.processor.parse_bytes(&mut *self.model.lock(),
                &batch.bytes[..batch.historical_bytes], &mut std::io::sink());
        }
        if batch.historical_bytes < batch.bytes.len() {
            self.end_historical_events();
            self.processor.parse_bytes(&mut *self.model.lock(),
                &batch.bytes[batch.historical_bytes..], &mut replies);
        }
        self.state.commit(batch)?;
        if self.state.phase() != ReplayPhase::Replaying {
            self.end_historical_events();
        }
        self.events.send_wakeup_event();
        Ok(replies)
    }

    fn end_historical_events(&mut self) {
        if self.historical_events_open {
            self.events.send_app_event(Event::PersistentReplayState { replaying: false });
            self.historical_events_open = false;
        }
    }
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
