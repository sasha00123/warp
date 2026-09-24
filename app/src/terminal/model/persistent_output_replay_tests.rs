//! Native block-model acceptance: replay has no local keypress/start-command
//! event. Shell integration alone must reconstruct the remote execution state.
use super::*;
use crate::terminal::model::ansi::{CompletionMetadata, Handler, PreexecValue, CommandFinishedValue, PrecmdValue, PromptMetadata};
use crate::terminal::model::block::{BlockId, BlockState};
use crate::terminal::model::test_utils::block_size;
use crate::terminal::SizeUpdate;
use warp_core::command::ExitCode;
use warp_core::features::FeatureFlag;

#[test]
fn persistent_output_replay_reconstructs_running_command_without_local_input() {
    for recovery in [false, true] {
        let _flag = FeatureFlag::TerminalLifecycleRecovery.override_enabled(recovery);
        let mut terminal = TerminalModel::mock(None, None);
        terminal.enable_persistent_workspace_mode();
        terminal.preexec(PreexecValue {
            command: "while true; do date; sleep 15; done".into(), session_id: None,
        });
        assert_eq!(terminal.block_list().active_block().state(), BlockState::Executing);
        assert_eq!(terminal.block_list().active_block().command_to_string(), "while true; do date; sleep 15; done");
    }
}

#[test]
fn persistent_output_replay_completion_then_another_command_preserves_block_boundary() {
    let _flag = FeatureFlag::TerminalLifecycleRecovery.override_enabled(true);
    let mut terminal = TerminalModel::mock(None, None);
    terminal.enable_persistent_workspace_mode();
    terminal.preexec(PreexecValue { command: "first".into(), session_id: None });
    let first = terminal.active_block_id().clone();
    let completion = CompletionMetadata { exit_code: ExitCode::from(7), next_block_id: BlockId::new() };
    terminal.command_finished(CommandFinishedValue { completion_metadata: completion.clone(), session_id: None });
    terminal.precmd_with_completion_metadata(PrecmdValue {
        completion_metadata: completion,
        prompt_metadata: PromptMetadata::default(),
    });
    assert_ne!(terminal.active_block_id(), &first);
    assert_eq!(terminal.block_list().previous_command_exit_code(), Some(ExitCode::from(7)));
    terminal.preexec(PreexecValue { command: "second still running".into(), session_id: None });
    assert_eq!(terminal.block_list().active_block().state(), BlockState::Executing);
    assert_eq!(terminal.block_list().active_block().command_to_string(), "second still running");
}

#[test]
fn persistent_output_replay_keeps_existing_command_text_and_rejects_repeated_preexec() {
    let mut terminal = TerminalModel::mock(None, None);
    terminal.enable_persistent_workspace_mode();
    terminal.block_list_mut().active_block_mut().init_command(b"my-alias");
    terminal.preexec(PreexecValue { command: "expanded-command".into(), session_id: None });
    assert_eq!(terminal.block_list().active_block().command_to_string(), "my-alias");
    terminal.preexec(PreexecValue { command: "late stale command".into(), session_id: None });
    assert_eq!(terminal.block_list().active_block().command_to_string(), "my-alias");
}

#[test]
fn persistent_output_replay_fallback_is_not_enabled_for_ordinary_terminals() {
    let mut terminal = TerminalModel::mock(None, None);
    terminal.preexec(PreexecValue { command: "not-local-editor-text".into(), session_id: None });
    assert!(terminal.block_list().active_block().is_command_empty());
}

#[test]
fn persistent_output_replay_multiline_commands_start_at_the_original_column() {
    for newline in ["\n", "\r\n"] {
        for recovery in [false, true] {
            let _flag = FeatureFlag::TerminalLifecycleRecovery.override_enabled(recovery);
            let mut terminal = TerminalModel::mock(None, None);
            terminal.enable_persistent_workspace_mode();
            terminal.preexec(PreexecValue {
                command: format!("printf first{newline}printf second"), session_id: None,
            });
            assert_eq!(terminal.block_list().active_block().command_to_string(),
                "printf first\nprintf second");
        }
    }
}

#[test]
fn persistent_output_replay_real_bash_multiline_echo_preserves_layout() {
    let recorded = include_bytes!("testdata/persistent_multiline_bash.output");
    for chunk_size in [1, 7, 257, recorded.len()] {
        for recovery in [false, true] {
            let _flag = FeatureFlag::TerminalLifecycleRecovery.override_enabled(recovery);
            let mut terminal = TerminalModel::mock(None, None);
            terminal.resize(SizeUpdate::from_cell_dimensions(block_size().size, 39, 123));
            terminal.enable_persistent_workspace_mode();
            terminal.register_session_id(SessionId::from(7600061777897549979_u64));
            let mut parser = crate::terminal::model::ansi::Processor::new();
            for bytes in recorded.chunks(chunk_size) {
                parser.parse_bytes(&mut terminal, bytes, &mut std::io::sink());
            }
            let block = terminal.block_list().blocks().iter().find(|block| {
                block.command_to_string().starts_with("printf '%s\\n' 'first'")
            }).expect("Recorded multiline command was not reconstructed");
            assert_eq!(block.command_to_string(), "printf '%s\\n' 'first'\nprintf '%s\\n' 'second'",
                "chunk_size={chunk_size}, recovery={recovery}");
            assert!(block.output_to_string().contains("first\nsecond"));
        }
    }
}
